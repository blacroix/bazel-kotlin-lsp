//! Pointing kotlin-lsp at the generated model through the client's `initialize` request.

use serde_json::{Value, json};

/// Adds `{"type": "json", "path": model_uri}` to `params.initializationOptions.projects`, keeping everything the
/// client sent. Returns false, leaving `params` alone, when the client's options are not a JSON object.
pub fn inject_project(params: &mut Value, model_uri: &str) -> bool {
    let entry = json!({"type": "json", "path": model_uri});
    let Some(params) = params.as_object_mut() else {
        return false;
    };
    let options = params.entry("initializationOptions").or_insert(Value::Null);
    if options.is_null() {
        *options = json!({});
    }
    let Some(options) = options.as_object_mut() else {
        return false;
    };
    let projects = options.entry("projects").or_insert_with(|| json!([]));
    if !projects.is_array() {
        *projects = json!([]);
    }
    let projects = projects.as_array_mut().expect("just made an array");
    if !projects.contains(&entry) {
        projects.push(entry);
    }
    true
}

/// Sets `params.initializationOptions.indexDir` to `dir` unless the client chose one. Call after
/// [`inject_project`] succeeded, which makes the options an object.
pub fn inject_index_dir(params: &mut Value, dir: &std::path::Path) {
    if let Some(options) = params
        .get_mut("initializationOptions")
        .and_then(Value::as_object_mut)
    {
        options
            .entry("indexDir")
            .or_insert_with(|| json!(dir.to_string_lossy()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URI: &str = "file:///cache/model";

    fn injected(mut params: Value) -> Value {
        assert!(inject_project(&mut params, URI));
        params
    }

    #[test]
    fn missing_or_null_options_get_the_project() {
        let expected = json!({"type": "json", "path": URI});
        for params in [
            json!({"rootUri": "file:///r"}),
            json!({"initializationOptions": null}),
        ] {
            assert_eq!(
                injected(params)["initializationOptions"],
                json!({"projects": [expected]})
            );
        }
    }

    #[test]
    fn other_keys_are_kept_and_projects_appended() {
        let params = injected(json!({
            "rootUri": "file:///r",
            "initializationOptions": {"indexDir": "/i", "projects": [{"type": "json", "path": "file:///other"}]}
        }));
        assert_eq!(params["rootUri"], "file:///r");
        assert_eq!(
            params["initializationOptions"],
            json!({"indexDir": "/i", "projects": [
                {"type": "json", "path": "file:///other"},
                {"type": "json", "path": URI}
            ]})
        );
    }

    #[test]
    fn the_same_project_is_not_added_twice() {
        let params = injected(injected(json!({})));
        assert_eq!(
            params["initializationOptions"]["projects"],
            json!([{"type": "json", "path": URI}])
        );
    }

    #[test]
    fn non_object_options_are_left_alone() {
        let mut params = json!({"initializationOptions": "x"});
        assert!(!inject_project(&mut params, URI));
        assert_eq!(params, json!({"initializationOptions": "x"}));
    }

    #[test]
    fn index_dir_is_set_unless_the_client_chose_one() {
        let mut params = injected(json!({}));
        inject_index_dir(&mut params, std::path::Path::new("/cache/idx"));
        assert_eq!(params["initializationOptions"]["indexDir"], "/cache/idx");
        let mut mine = injected(json!({"initializationOptions": {"indexDir": "/mine"}}));
        inject_index_dir(&mut mine, std::path::Path::new("/cache/idx"));
        assert_eq!(mine["initializationOptions"]["indexDir"], "/mine");
    }
}
