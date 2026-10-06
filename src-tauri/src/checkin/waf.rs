//! Deciding whether a page is a human-verification challenge.
//!
//! # Why this decides anything at all
//!
//! Check-in on a panel behind a WAF needs a real browser, and the reason it needs
//! one is that the challenge cannot be solved by a script. So the operation
//! reaches a state where a person has to do something. The only question this
//! module answers is whether it has reached that state — and getting it wrong in
//! either direction is expensive:
//!
//! * **Reading a normal page as a challenge** discards a working session and asks
//!   the user to authenticate again, for no reason.
//! * **Reading a challenge as a normal page** reports a check-in that did not
//!   happen as one that did, which is the specific fabrication the rest of the
//!   check-in design exists to prevent.
//!
//! # Why the evidence is structured rather than a string match
//!
//! Text on a challenge page is localised, rebranded and reworded by WAF vendors
//! and by whoever configured the WAF, so matching a phrase is unreliable in both
//! directions. What is *not* localisable is the mechanism: the interstitial
//! stops the page from reaching the application, and it loads a cross-origin
//! widget from the vendor's own domain. So the primary signal is
//! [`PageEvidence::challenge_widget`] — a cross-origin frame or script the page's
//! own application would never load — and the text is a secondary corroboration
//! only.
//!
//! Being wrong in the cautious direction is also handled: when the evidence is
//! ambiguous the verdict is [`Verdict::Indeterminate`] rather than either
//! answer, and the caller treats that as "keep watching" instead of deciding.

/// What could be learned about the loaded page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageEvidence {
    /// The URL the browser is actually on.
    pub url: String,
    /// The document title.
    pub title: String,
    /// Visible text, truncated by the caller.
    pub body_text: String,
    /// Hosts of every cross-origin frame the page has loaded.
    pub frame_hosts: Vec<String>,
    /// `true` when the page is the application's own document rather than a
    /// challenge interstitial.
    ///
    /// This is the strongest signal available and it is negative: the panel's own
    /// page proves the WAF let us through. A WAF vendor serving a challenge
    /// cannot also be serving the application.
    pub application_dom_loaded: bool,
    /// Seconds spent waiting for the page to settle.
    pub elapsed_secs: f64,
}

/// What the evidence says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The page is the application's own document.
    Clear,
    /// The page is a human-verification challenge.
    Challenge,
    /// The evidence does not establish either.
    ///
    /// Deliberately not folded into [`Verdict::Clear`]: "the WAF let us through" and
    /// "we cannot tell yet" are different claims, and treating the second as the
    /// first would report a check-in that never ran as one that did.
    Indeterminate,
}

impl Verdict {
    /// Whether this verdict should pause the operation and ask the user.
    pub fn needs_user(self) -> bool {
        matches!(self, Verdict::Challenge)
    }
}

/// Hosts that serve a human-verification widget.
///
/// Presence is a signal, not proof: a panel can legitimately embed one of these
/// for its own sign-in. What it cannot do is serve the application document *and*
/// the widget's iframe from the same page — which is why
/// [`PageEvidence::application_dom_loaded`] outranks a widget seen alone.
const CHALLENGE_HOSTS: &[&str] = &[
    "challenges.cloudflare.com",
    "cloudflareinsights.com",
    "hcaptcha.com",
    "js.hcaptcha.com",
    "gstatic.com",
    "recaptcha.net",
    "www.google.com",
    "perfdigest.net",
    "geetest.com",
];

/// Read the evidence.
///
/// The order of the checks is the policy, so it is written out rather than
/// folded into a score:
///
/// 1. The application's own document is on screen. A WAF that let the request
///    through is not challenging it, whatever else the page contains.
/// 2. A vendor widget is loaded from a cross-origin frame and the application
///    never arrived.
/// 3. Corroborating text, only to break a tie between 2 and indeterminate.
pub fn classify(evidence: &PageEvidence) -> Verdict {
    if evidence.application_dom_loaded {
        return Verdict::Clear;
    }
    if evidence.elapsed_secs < 0.0 {
        // A nonsensical elapsed time means the evidence was assembled wrong;
        // deciding from it would be deciding from a measurement that did not
        // happen.
        return Verdict::Indeterminate;
    }
    if !challenge_widget(&evidence.frame_hosts) {
        // No vendor widget and no application document: whatever is on screen is
        // something else — a 404, a proxy error, an empty body. Calling that a
        // challenge would put the user in front of a browser showing nothing, and
        // calling it clear would claim the panel answered.
        return Verdict::Indeterminate;
    }
    if mentions_verification(&evidence.body_text) || mentions_verification(&evidence.title) {
        return Verdict::Challenge;
    }
    Verdict::Indeterminate
}

/// Whether any loaded frame belongs to a verification vendor.
fn challenge_widget(frame_hosts: &[String]) -> bool {
    frame_hosts.iter().any(|host| {
        let host = host.trim().to_ascii_lowercase();
        CHALLENGE_HOSTS
            .iter()
            .any(|known| host == *known || host.ends_with(&format!(".{known}")))
    })
}

/// Whether the text corroborates a verification challenge.
///
/// Secondary and deliberately narrow. Kept short and case-insensitive, and only
/// ever consulted when a vendor widget is already loaded — so a false positive
/// here cannot by itself turn a normal page into a challenge.
fn mentions_verification(text: &str) -> bool {
    let lowered = text.to_lowercase();
    [
        "verify you are human",
        "verifying you are human",
        "are you a robot",
        "checking your browser",
        "just a moment",
        "请完成安全验证",
        "安全验证",
        "人机验证",
        "cf-challenge",
        "challenge-platform",
    ]
    .iter()
    .any(|token| lowered.contains(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts(list: &[&str]) -> Vec<String> {
        list.iter().map(|h| (*h).to_string()).collect()
    }

    #[test]
    fn the_applications_own_page_is_always_clear() {
        // Even with a vendor widget elsewhere on it: the application document
        // proves the WAF admitted the request.
        let evidence = PageEvidence {
            url: "https://relay.example/console/personal".into(),
            title: "Personal".into(),
            body_text: "Check in".into(),
            frame_hosts: hosts(&["challenges.cloudflare.com"]),
            application_dom_loaded: true,
            elapsed_secs: 3.0,
        };
        assert_eq!(classify(&evidence), Verdict::Clear);
        assert!(!Verdict::Clear.needs_user());
    }

    #[test]
    fn a_vendor_interstitial_with_verification_text_is_a_challenge() {
        let evidence = PageEvidence {
            url: "https://relay.example/console/personal".into(),
            title: "Just a moment...".into(),
            body_text: "Checking your browser before accessing".into(),
            frame_hosts: hosts(&["challenges.cloudflare.com"]),
            application_dom_loaded: false,
            elapsed_secs: 2.0,
        };
        assert_eq!(classify(&evidence), Verdict::Challenge);
        assert!(Verdict::Challenge.needs_user());
    }

    #[test]
    fn a_localised_challenge_is_still_a_challenge() {
        // WAF text is localised and reworded; the widget is the durable signal.
        for title in ["请完成安全验证", "人机验证", "Verifying you are human"] {
            let evidence = PageEvidence {
                title: title.into(),
                body_text: String::new(),
                frame_hosts: hosts(&["challenges.cloudflare.com"]),
                application_dom_loaded: false,
                elapsed_secs: 2.0,
                ..Default::default()
            };
            assert_eq!(classify(&evidence), Verdict::Challenge, "title {title:?}");
        }
    }

    #[test]
    fn a_widget_with_no_recognisable_text_is_indeterminate_not_clear() {
        // The vendor changed its wording. Guessing "clear" here would report a
        // check-in that never ran as one that did.
        let evidence = PageEvidence {
            frame_hosts: hosts(&["challenges.cloudflare.com"]),
            application_dom_loaded: false,
            elapsed_secs: 2.0,
            ..Default::default()
        };
        assert_eq!(classify(&evidence), Verdict::Indeterminate);
        assert!(!Verdict::Indeterminate.needs_user());
    }

    #[test]
    fn an_empty_document_is_indeterminate_not_a_challenge() {
        // A proxy error page or an empty response is not a challenge, and putting
        // the user in front of a browser showing nothing is not a fix.
        let evidence = PageEvidence {
            frame_hosts: Vec::new(),
            body_text: String::new(),
            application_dom_loaded: false,
            elapsed_secs: 2.0,
            ..Default::default()
        };
        assert_eq!(classify(&evidence), Verdict::Indeterminate);
    }

    #[test]
    fn a_404_page_is_not_a_challenge() {
        let evidence = PageEvidence {
            frame_hosts: Vec::new(),
            body_text: "404 Not Found".into(),
            application_dom_loaded: false,
            elapsed_secs: 1.0,
            ..Default::default()
        };
        assert_eq!(classify(&evidence), Verdict::Indeterminate);
    }

    #[test]
    fn verification_text_alone_is_not_enough() {
        // A panel can legitimately use these words on its own sign-in page. Only a
        // vendor widget, with the application absent, means a WAF.
        let evidence = PageEvidence {
            title: "Sign in".into(),
            body_text: "Verify you are human to continue".into(),
            frame_hosts: Vec::new(),
            application_dom_loaded: false,
            elapsed_secs: 1.0,
            ..Default::default()
        };
        assert_eq!(classify(&evidence), Verdict::Indeterminate);
    }

    #[test]
    fn a_subdomain_of_a_vendor_host_still_counts() {
        let evidence = PageEvidence {
            body_text: "Just a moment".into(),
            frame_hosts: hosts(&["a.b.challenges.cloudflare.com"]),
            application_dom_loaded: false,
            elapsed_secs: 1.0,
            ..Default::default()
        };
        assert_eq!(classify(&evidence), Verdict::Challenge);
    }

    #[test]
    fn a_host_that_merely_contains_a_vendor_name_does_not_count() {
        // Suffix matching, not substring matching: a relay hosted at
        // `challenges.cloudflare.com.example.net` is not Cloudflare.
        assert!(!challenge_widget(&hosts(&[
            "challenges.cloudflare.com.example.net"
        ])));
        assert!(!challenge_widget(&hosts(&[
            "notchallenges.cloudflare.com.evil"
        ])));
        assert!(challenge_widget(&hosts(&["challenges.cloudflare.com"])));
    }

    #[test]
    fn host_matching_ignores_case_and_surrounding_space() {
        assert!(challenge_widget(&hosts(&["  Challenges.CloudFlare.com  "])));
    }

    #[test]
    fn a_nonsensical_elapsed_time_yields_no_verdict() {
        let evidence = PageEvidence {
            frame_hosts: hosts(&["challenges.cloudflare.com"]),
            body_text: "Just a moment".into(),
            application_dom_loaded: false,
            elapsed_secs: -1.0,
            ..Default::default()
        };
        assert_eq!(classify(&evidence), Verdict::Indeterminate);
    }
}
