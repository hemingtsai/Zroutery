//! A fresh Runtime for every call. No ambient callbacks or persisted VM globals.
use crate::loader::PackageLoader;
use rquickjs::{Context, Function, Module, Runtime};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use zroutery_plugin_api::{
    ipc::{WorkerBootstrap, WorkerCall, WorkerReply},
    strict, PluginError, Result,
};

pub fn execute(bootstrap: &WorkerBootstrap, call: &WorkerCall) -> Result<WorkerReply> {
    let limits = bootstrap.limits.bounded()?;
    if bootstrap.modules.len() > 64
        || bootstrap.modules.values().map(String::len).sum::<usize>() > 192 * 1024
        || !strict::package_path(&bootstrap.entry)
        || !bootstrap.entry.ends_with(".mjs")
    {
        return Err(PluginError::InvalidManifest);
    }
    let source = bootstrap
        .modules
        .get(&bootstrap.entry)
        .ok_or(PluginError::InvalidManifest)?;
    let runtime = Runtime::new().map_err(|_| PluginError::PluginMemoryLimit)?;
    runtime.set_memory_limit(limits.heap_bytes);
    runtime.set_max_stack_size(limits.stack_bytes);
    let expired = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_millis(limits.active_js_ms);
    let timeout = Arc::clone(&expired);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        if Instant::now() >= deadline {
            timeout.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    })));
    let modules = Arc::new(bootstrap.modules.clone());
    runtime.set_loader(PackageLoader(Arc::clone(&modules)), PackageLoader(modules));
    let context = Context::full(&runtime).map_err(|_| PluginError::PluginMemoryLimit)?;
    let input = serde_json::to_string(call).map_err(|_| PluginError::InvalidResult)?;
    let reply = context.with(|ctx| -> rquickjs::Result<String> {
        // Capture pristine serializers before importing untrusted code. Reading
        // getters/toJSON and serializing the result remains inside the VM budget.
        let dispatch: Function = ctx.eval(r#"(() => {
          const parse = JSON.parse.bind(JSON), stringify = JSON.stringify.bind(JSON);
          const freeze = Object.freeze.bind(Object);
          return (hooks, json) => {
            const call = parse(json), input = call.input;
            const context = freeze({limits: freeze({}), cancelled: () => false});
            let result;
            switch (call.kind) {
              case 'validate_options': result = {kind:'options_validation', result:hooks.validate_options(input, context)}; break;
              case 'migrate_options': result = {kind:'options_migration', result:hooks.migrate_options(input, context)}; break;
              case 'ui_render': result = {kind:'ui_tree', result:hooks.ui_render[call.hook](input, context)}; break;
              case 'ui_action': result = {kind:'ui_effect', result:hooks.ui_action[call.hook](input, context)}; break;
              default: throw new Error('invalid_method');
            }
            return stringify(result);
          };
        })()"#)?;
        let (module, promise) = Module::declare(ctx.clone(), bootstrap.entry.as_str(), source.as_bytes())?.eval()?;
        // Synchronous hooks: top-level await is not a way to keep the worker alive.
        promise.finish::<()>()?;
        let hooks: rquickjs::Object = module.get("default")?;
        dispatch.call((hooks, input.as_str()))
    });
    let result = match reply {
        Ok(json) => strict::decode(json.as_bytes(), strict::MAX_FRAME_BYTES),
        Err(_) if expired.load(Ordering::Relaxed) => Err(PluginError::PluginTimeout),
        Err(_) => Err(PluginError::InvalidResult),
    };
    // Promise jobs never outlive a call. Dropping Context/Runtime discards any
    // queued work; no asynchronous I/O exists to survive it.
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use zroutery_plugin_api::{
        ipc::{Locale, UiInput},
        ui::Projection,
        ResourceLimits,
    };
    fn bootstrap(source: &str) -> WorkerBootstrap {
        WorkerBootstrap {
            entry: "ui.mjs".into(),
            modules: BTreeMap::from([("ui.mjs".into(), source.into())]),
            limits: ResourceLimits::default(),
            diagnostics: vec![],
        }
    }
    fn call() -> WorkerCall {
        WorkerCall::UiRender {
            hook: "test".into(),
            input: UiInput {
                contribution_id: "test".into(),
                locale: Locale::En,
                view_revision: "1".into(),
                projection: Projection::default(),
            },
        }
    }
    #[test]
    fn esm_has_no_network_os_dom_invoke_or_secret_globals() {
        let script = "import {define_plugin} from 'zroutery:plugin/v1'; export default define_plugin({ui_render:{test:()=>({kind:'text',text:[typeof fetch,typeof process,typeof Deno,typeof Bun,typeof document,typeof __TAURI_INTERNALS__,typeof std,typeof os].join(',')})}});";
        let WorkerReply::UiTree {
            result: zroutery_plugin_api::ui::UiNode::Text { text },
        } = execute(&bootstrap(script), &call()).unwrap()
        else {
            panic!("wrong result")
        };
        assert_eq!(
            text,
            "undefined,undefined,undefined,undefined,undefined,undefined,undefined,undefined"
        );
    }
    #[test]
    fn runaway_code_is_interrupted_and_modules_cannot_escape() {
        let script = "export default {ui_render:{test:()=>{while(true){} }}}";
        assert_eq!(
            execute(&bootstrap(script), &call()),
            Err(PluginError::PluginTimeout)
        );
        for name in [
            "std",
            "os",
            "https://evil.example/x.mjs",
            "file:///x.mjs",
            "../../x.mjs",
            "node:fs",
        ] {
            let source = format!("import '{name}'; export default {{}};");
            assert!(execute(&bootstrap(&source), &call()).is_err(), "{name}");
        }
    }
    #[test]
    fn every_call_gets_a_new_heap_and_module_state() {
        let script = "let n=0; export default {ui_render:{test:()=>({kind:'value',label:'n',value:String(++n)})}}";
        let setup = bootstrap(script);
        assert_eq!(
            execute(&setup, &call()).unwrap(),
            execute(&setup, &call()).unwrap()
        );
    }
}
