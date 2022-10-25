use bindings::{backend, cache, origin};

struct Component;

impl backend::Backend for Component {
    fn fetch(url: String) -> Vec<u8> {
        if let Some(data) = cache::get(&url) {
            return data;
        }

        let data = origin::fetch(&url);
        cache::put(&url, &data);
        data
    }
}

bindings::export!(Component);
