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
    pub fn add(&mut self, pattern: &str, handler: HttpHandler) -> String {
        let pattern = normalize_route_path(pattern);
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.pattern == pattern)
        {
            entry.handler = handler;
        } else {
            self.entries.push(RouteEntry {
                pattern: pattern.clone(),
                handler,
            });
        }
        pattern
    }

    pub fn resolve(&self, _method: &Method, uri: &Uri) -> Option<(HttpHandler, Vec<String>)> {
        self.entries.iter().find_map(|entry| {
            match_route(&entry.pattern, uri.path())
                .map(|params| (Arc::clone(&entry.handler), params))
        })
    }
}

pub(super) fn normalize_route_path(path: &str) -> String {
    let path = path.trim();
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
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
            || (pattern.starts_with('{') && pattern.ends_with('}'))
            || (pattern.starts_with(':'));
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
    use super::{match_route, DynamicRouter};
    use axum::http::{Method, Uri};
    use serde_json::json;
    use std::sync::Arc;

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

    #[test]
    fn normalizes_missing_leading_slash() {
        let mut router = DynamicRouter::default();
        router.add("api/hello", Arc::new(|_, _| Ok(json!({"handler": 1}))));
        let uri: Uri = "/api/hello".parse().unwrap();
        assert!(router.resolve(&Method::GET, &uri).is_some());
    }

    #[test]
    fn duplicate_registration_replaces_handler() {
        let mut router = DynamicRouter::default();
        router.add("/api/hello", Arc::new(|_, _| Ok(json!({"handler": 1}))));
        router.add("/api/hello", Arc::new(|_, _| Ok(json!({"handler": 2}))));

        let uri: Uri = "/api/hello".parse().unwrap();
        let (handler, params) = router.resolve(&Method::GET, &uri).unwrap();
        assert_eq!(handler(None, params).unwrap(), json!({"handler": 2}));
    }
}
