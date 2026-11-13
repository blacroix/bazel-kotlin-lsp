"""bazel-kotlin-lsp aspect: one JSON file per JVM target, read by `bazel-kotlin-lsp sync` to build kotlin-lsp's model.

Rendered at sync time: %{rules_java} and %{rules_kotlin} become canonical repository names, because an injected
repository has no repository mapping of its own.
"""

load("@@%{rules_java}//java/common:java_info.bzl", "JavaInfo")
load("@@%{rules_kotlin}//kotlin/internal:defs.bzl", "KtCompilerPluginInfo", _KOTLIN_TOOLCHAIN = "TOOLCHAIN_TYPE")
load("@@%{rules_kotlin}//src/main/starlark/core/options:opts.kotlinc.bzl", "KotlincOptions")

_DEP_ATTRS = ["deps", "exports", "associates"]
_IMPORT_KINDS = ["jvm_import", "kt_jvm_import", "java_import"]
_SOURCE_EXTENSIONS = ["kt", "java"]
_JAVA_RUNTIME_TOOLCHAIN = "@bazel_tools//tools/jdk:runtime_toolchain_type"

BklInfo = provider(doc = "Aspect outputs of a target and its dependencies.", fields = {"files": "depset of File"})

def _as_targets(value):
    if value == None:
        return []
    if type(value) == "Target":
        return [value]
    if type(value) == "list":
        return [v for v in value if type(v) == "Target"]
    if type(value) == "dict":
        return [k for k in value.keys() if type(k) == "Target"]
    return []

def _attr_targets(ctx, attr):
    return _as_targets(getattr(ctx.rule.attr, attr, None))

def _rule_files(ctx, attr):
    return getattr(ctx.rule.files, attr, None) or []

def _paths(files):
    return [f.path for f in files]

def _option_key(option):
    # rules_kotlin 2.1 names the key `id`; later versions name it `key`.
    return getattr(option, "key", None) or getattr(option, "id", "")

def _kotlinc(ctx):
    toolchain = ctx.toolchains[_KOTLIN_TOOLCHAIN]
    if not ctx.rule.kind.startswith("kt_"):
        return None
    opts = None
    opts_targets = _attr_targets(ctx, "kotlinc_opts")
    if opts_targets and KotlincOptions in opts_targets[0]:
        opts = opts_targets[0][KotlincOptions]
    elif toolchain:
        opts = getattr(toolchain, "kotlinc_options", None)
    classpath = []
    options = []
    for plugin in _attr_targets(ctx, "plugins"):
        if KtCompilerPluginInfo not in plugin:
            continue
        info = plugin[KtCompilerPluginInfo]
        if not getattr(info, "compile", True):
            continue
        classpath.extend(_paths(info.classpath.to_list()))
        options.extend(["plugin:%s:%s=%s" % (info.id, _option_key(o), o.value) for o in (info.options or [])])
    return {
        "language_version": getattr(toolchain, "language_version", None) if toolchain else None,
        "api_version": getattr(toolchain, "api_version", None) if toolchain else None,
        "jvm_target": getattr(toolchain, "jvm_target", None) if toolchain else None,
        "opt_in": list(getattr(opts, "x_optin", None) or []) if opts else [],
        "plugin_classpath": classpath,
        "plugin_options": options,
    }

def _java_home(ctx):
    runtime = ctx.toolchains[_JAVA_RUNTIME_TOOLCHAIN]
    if not runtime or not hasattr(runtime, "java_runtime"):
        return None
    return runtime.java_runtime.java_home

def _bkl_aspect_impl(target, ctx):
    transitive = [d[BklInfo].files for attr in _DEP_ATTRS for d in _attr_targets(ctx, attr) if BklInfo in d]
    if JavaInfo not in target:
        files = depset(transitive = transitive)
        return [BklInfo(files = files), OutputGroupInfo(bkl_json = files, bkl_jars = depset())]

    java_info = target[JavaInfo]
    srcs = _rule_files(ctx, "srcs")
    sources = [f for f in srcs if f.is_source and f.extension in _SOURCE_EXTENSIONS]
    generated = [f for f in srcs if (not f.is_source and f.extension in _SOURCE_EXTENSIONS) or f.extension == "srcjar"]
    is_import = ctx.rule.kind in _IMPORT_KINDS
    class_jars = [o.class_jar for o in java_info.java_outputs if o.class_jar]
    source_jars = java_info.source_jars
    info = {
        "label": str(target.label),
        "repo": target.label.repo_name,
        "package": target.label.package,
        "name": target.label.name,
        "kind": ctx.rule.kind,
        "srcs": _paths(sources),
        "generated_srcs": _paths(generated),
        "deps": [str(t.label) for t in _attr_targets(ctx, "deps")],
        "exports": [str(t.label) for t in _attr_targets(ctx, "exports")],
        "associates": [str(t.label) for t in _attr_targets(ctx, "associates")],
        "class_jars": _paths(class_jars),
        "source_jars": _paths(source_jars),
        "import_jars": _paths(_rule_files(ctx, "jars") + _rule_files(ctx, "jar")) if is_import else [],
        "import_srcjar": _paths(_rule_files(ctx, "srcjar")) if is_import else [],
        "tags": list(getattr(ctx.rule.attr, "tags", None) or []),
        "kotlinc": _kotlinc(ctx),
        "java_home": _java_home(ctx),
    }
    out = ctx.actions.declare_file(target.label.name + ".bkl.json")
    ctx.actions.write(out, json.encode(info))

    files = depset([out] + [f for f in generated if f.extension in _SOURCE_EXTENSIONS], transitive = transitive)
    jars = depset([] if is_import else class_jars + source_jars)
    return [BklInfo(files = files), OutputGroupInfo(bkl_json = files, bkl_jars = jars)]

bkl_aspect = aspect(
    implementation = _bkl_aspect_impl,
    attr_aspects = _DEP_ATTRS,
    toolchains = [
        config_common.toolchain_type(_JAVA_RUNTIME_TOOLCHAIN, mandatory = False),
        config_common.toolchain_type(_KOTLIN_TOOLCHAIN, mandatory = False),
    ],
)
