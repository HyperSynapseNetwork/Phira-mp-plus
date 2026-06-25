use axum::http::{Method, Uri};
use serde_json::Value;
use std::sync::Arc;

pub type HttpHandler =
    Arc<dyn Fn(Option<Value>, Vec<String>) -> Result<Value, (u16, String)> + Send + Sync>;

struct RouteEntry {
    pattern: String,
    handler: HttpHandler,
}

#[derive(Default)]
pub struct DynamicRouter {
    entries: Vec<RouteEntry>,
}

impl DynamicRouter {
    pub fn add(&mut self, pattern: String, handler: HttpHandler) {
        self.entries.push(RouteEntry { pattern, handler });
    }

    pub fn resolve(&self, _method: &Method, uri: &Uri) -> Option<(HttpHandler, Vec<String>)> {
        self.entries.iter().find_map(|entry| {
            match_route(&entry.pattern, uri.path())
                .map(|params| (Arc::clone(&entry.handler), params))
        })
    }
}

fn match_route(pattern: &str, path: &str) -> Option<Vec<String>> {
    let pattern_segments: Vec<_> = pattern.split('/').collect();
    let path_segments: Vec<_> = path.split('/').collect();
    if pattern_segments.len() != path_segments.len() {
        return None;
    }

    let mut params = Vec::new();
    for (pattern, value) in pattern_segments.iter().zip(path_segments) {
        let parameter = (pattern.starts_with('<') && pattern.ends_with('>'))
            || (pattern.starts_with('{') && pattern.ends_with('}'));
        if parameter {
            params.push(value.to_string());
        } else if *pattern != value {
            return None;
        }
    }
    Some(params)
}

#[cfg(test)]
mod tests {
    use super::match_route;

    #[test]
    fn extracts_angle_and_brace_parameters() {
        assert_eq!(
            match_route("/rooms/<room>/users/{user}", "/rooms/alpha/users/42"),
            Some(vec!["alpha".to_string(), "42".to_string()])
        );
    }

    #[test]
    fn rejects_different_paths() {
        assert_eq!(match_route("/rooms/<room>", "/users/alpha"), None);
    }
}
