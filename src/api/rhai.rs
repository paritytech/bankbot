use rhai::plugin::*;

#[export_module]
pub mod env {
    pub type Vars = std::collections::HashMap<String, String>;

    pub fn get(name: &str) -> String {
        std::env::var(name).unwrap_or("".into())
    }
}
