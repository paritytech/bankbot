use rhai::plugin::*;

#[export_module]
pub mod env {
    pub type Vars = std::collections::HashMap<String, String>;

    pub fn get(name: &str) -> String {
        std::env::var(name).unwrap_or("".into())
    }
}

#[export_module]
pub mod toml {
    /// Patch the relative dependencies (`{ path = "../../bla", ... }`) in the given TOML to the
    /// given git url and ref.
    #[rhai_fn(return_raw)]
    pub fn replace_path_dependencies_with_git(toml: Vec<u8>, url: String, branch: String) -> Result <rhai::Blob, Box<rhai::EvalAltResult>> {
        use toml_edit::{Document, Item, Value};
        let toml = String::from_utf8(toml).map_err(|_| format!("toml is invalid UTF8"))?;
        let mut doc = toml.parse::<Document>().map_err(|_| format!("Not a valid toml document"))?;

        for table in ["dependencies", "build-dependencies", "dev-dependencies"] {
            println!("processing {table}");
            let deps = match doc.entry(table) {
                toml_edit::Entry::Occupied(entry) => match entry.into_mut() {
                    Item::Table(deps) => deps,
                    _ => continue,
                },
                toml_edit::Entry::Vacant(_entry) => continue,
            };

            for (_name, value) in deps.iter_mut() {
                match value {
                    Item::Table(dep) => {
                        if let Some(_) = dep.remove_entry("path") {
                            dep.insert("git", Item::Value(Value::from(url.clone())));
                            dep.insert("branch", Item::Value(Value::from(branch.clone())));
                        }
                    },
                    Item::Value(Value::InlineTable(dep)) => {
                        if let Some(_) = dep.remove_entry("path") {
                            dep.insert("git", Value::from(url.clone()));
                            dep.insert("branch", Value::from(branch.clone()));
                        }
                    },
                    _ => { println!("wtf is this? {:?}", value); continue},
                }
            }
        }

        Ok(doc.to_string().into_bytes())
    }
}
