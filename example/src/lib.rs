cargo_component_bindings::generate!();

use bindings::{
    example::component::{backend as origin, cache},
    exports::example::component::backend::Backend,
};

struct Component;

impl Backend for Component {
    fn fetch(url: String) -> Vec<u8> {
        if let Some(data) = cache::get(&url) {
            return data;
        }

        let data = origin::fetch(&url);
        cache::put(&url, &data);
        data
    }
}
