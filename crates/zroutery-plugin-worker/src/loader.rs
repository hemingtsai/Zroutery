use rquickjs::{
    loader::{ImportAttributes, Loader, Resolver},
    Ctx, Error, Module, Result,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use zroutery_plugin_api::strict::package_path;

pub struct PackageLoader(pub Arc<BTreeMap<String, String>>);
impl Resolver for PackageLoader {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> Result<String> {
        if attributes.is_some() {
            return Err(Error::new_resolving(base, name));
        }
        if name == "zroutery:plugin/v1" {
            return Ok(name.into());
        }
        if !name.starts_with("./") && !name.starts_with("../") {
            return Err(Error::new_resolving(base, name));
        }
        let mut segments: Vec<&str> = base.split('/').collect();
        segments.pop();
        for part in name.split('/') {
            match part {
                "." => {}
                ".." => {
                    if segments.pop().is_none() {
                        return Err(Error::new_resolving(base, name));
                    }
                }
                p => segments.push(p),
            }
        }
        let path = segments.join("/");
        if !package_path(&path) || !path.ends_with(".mjs") || !self.0.contains_key(&path) {
            return Err(Error::new_resolving(base, name));
        }
        Ok(path)
    }
}
impl Loader for PackageLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> Result<Module<'js>> {
        if attributes.is_some() {
            return Err(Error::new_loading(name));
        }
        if name == "zroutery:plugin/v1" {
            return Module::declare(
                ctx.clone(),
                name,
                b"export function define_plugin(hooks) { return Object.freeze(hooks); }",
            );
        }
        let source = self.0.get(name).ok_or_else(|| Error::new_loading(name))?;
        Module::declare(ctx.clone(), name, source.as_bytes())
    }
}
