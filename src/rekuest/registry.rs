use crate::App;

use super::api::create_template;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

pub struct FunctionRegistry {
    functions: HashMap<
        String,
        Box<dyn Fn((App, String)) -> Pin<Box<dyn Future<Output = String> + Send>> + Send + Sync>,
    >,
    templates: HashMap<String, create_template::TemplateInput>,
}

impl FunctionRegistry {
    pub fn new() -> Self {
        Self {
            functions: HashMap::new(),
            templates: HashMap::new(),
        }
    }

    pub fn register<F, Fut>(
        &mut self,
        name: &str,
        function: F,
        template: create_template::TemplateInput,
    ) where
        F: Fn(App, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = String> + Send + 'static,
    {
        // Wrap the given function into one returning a boxed, pinned future
        let wrapped =
            move |(app, input): (App, String)| -> Pin<Box<dyn Future<Output = String> + Send>> {
                Box::pin(function(app, input))
            };

        self.functions.insert(name.to_string(), Box::new(wrapped));
        self.templates.insert(name.to_string(), template);
    }

    pub fn get_function(
        &self,
        name: &str,
    ) -> Option<
        &Box<dyn Fn((App, String)) -> Pin<Box<dyn Future<Output = String> + Send>> + Send + Sync>,
    > {
        self.functions.get(name)
    }

    pub fn get_template(&self, name: &str) -> Option<&create_template::TemplateInput> {
        self.templates.get(name)
    }
}
