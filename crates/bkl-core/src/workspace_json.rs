//! kotlin-lsp's `workspace.json`, field for field (see the format notes in docs).

use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub modules: Vec<Module>,
    pub libraries: Vec<Library>,
    pub sdks: Vec<Sdk>,
    pub kotlin_settings: Vec<KotlinSettings>,
    pub java_settings: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Module {
    pub name: String,
    #[serde(rename = "type")]
    pub module_type: String,
    pub dependencies: Vec<Dependency>,
    pub content_roots: Vec<ContentRoot>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Dependency {
    Module { name: String, scope: String },
    Library { name: String, scope: String },
    ModuleSource,
    Sdk { name: String, kind: String },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContentRoot {
    pub path: String,
    pub source_roots: Vec<SourceRootJson>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceRootJson {
    pub path: String,
    #[serde(rename = "type")]
    pub root_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_prefix: Option<String>,
    pub generated: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Library {
    pub name: String,
    #[serde(rename = "type")]
    pub library_type: String,
    pub roots: Vec<LibraryRoot>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LibraryRoot {
    pub path: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub root_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Sdk {
    pub name: String,
    #[serde(rename = "type")]
    pub sdk_type: String,
    pub version: String,
    pub home_path: String,
    pub additional_data: String,
}

/// The IntelliJ Kotlin facet, with the field set kotlin-lsp's own export writes.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KotlinSettings {
    pub name: String,
    pub source_roots: Vec<String>,
    pub config_file_items: Vec<serde_json::Value>,
    pub module: String,
    pub use_project_settings: bool,
    pub implemented_module_names: Vec<String>,
    pub depends_on_module_names: Vec<String>,
    pub additional_visible_module_names: Vec<String>,
    pub production_output_path: Option<String>,
    pub test_output_path: Option<String>,
    pub source_set_names: Vec<String>,
    pub is_test_module: bool,
    pub external_project_id: String,
    pub is_hmpp_enabled: bool,
    pub pure_kotlin_source_folders: Vec<String>,
    pub kind: String,
    pub compiler_arguments: String,
    pub additional_arguments: Option<String>,
    pub script_templates: Option<String>,
    pub script_templates_classpath: Option<String>,
    #[serde(rename = "outputDirectoryForJsLibraryFiles")]
    pub output_directory_for_js_library_files: Option<String>,
    pub target_platform: Option<String>,
    pub external_system_run_tasks: Vec<serde_json::Value>,
    pub version: u32,
    pub flush_needed: bool,
}
