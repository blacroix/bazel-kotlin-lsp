//! A synthetic Bzlmod repository: N `local_path_override` modules × M Kotlin targets with cross-module deps.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The rule versions the synthetic repo uses; kept equal to the fixture's.
pub const RULE_VERSIONS: [(&str, &str); 2] = [("rules_kotlin", "2.4.21"), ("rules_java", "9.9.0")];

#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub modules: usize,
    pub targets_per_module: usize,
    pub seed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPlan {
    pub module: usize,
    pub index: usize,
    /// (module, index) pairs; always earlier in (module, index) order, so the graph is a DAG.
    pub deps: Vec<(usize, usize)>,
}

pub fn plan(spec: &Spec) -> Vec<TargetPlan> {
    let mut out = Vec::new();
    for module in 0..spec.modules {
        for index in 0..spec.targets_per_module {
            let mut deps = Vec::new();
            if index > 0 {
                deps.push((module, index - 1));
            }
            if module > 0 {
                let other = ((spec.seed as usize) * 31 + module * 7 + index) % module;
                let target = ((spec.seed as usize) + index * 13) % spec.targets_per_module;
                deps.push((other, target));
            }
            out.push(TargetPlan {
                module,
                index,
                deps,
            });
        }
    }
    out
}

fn module_name(m: usize) -> String {
    format!("m{m:02}")
}

fn target_name(i: usize) -> String {
    format!("t{i:03}")
}

fn class_name(i: usize) -> String {
    format!("T{i:03}")
}

fn fqcn(m: usize, i: usize) -> String {
    format!(
        "synth.{}.{}.{}",
        module_name(m),
        target_name(i),
        class_name(i)
    )
}

pub fn generate(spec: &Spec, out: &Path) -> anyhow::Result<()> {
    let targets = plan(spec);
    let write = |rel: &str, text: &str| -> anyhow::Result<()> {
        let path: PathBuf = out.join(rel);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, text)?;
        Ok(())
    };
    let rule_deps: String = RULE_VERSIONS
        .iter()
        .map(|(n, v)| format!("bazel_dep(name = \"{n}\", version = \"{v}\")\n"))
        .collect();

    let mut root = format!("module(name = \"synth\")\n\n{rule_deps}");
    for m in 0..spec.modules {
        let name = module_name(m);
        let _ = write!(
            root,
            "bazel_dep(name = \"{name}\")\nlocal_path_override(\n    module_name = \"{name}\",\n    path = \"mods/{name}\",\n)\n"
        );
    }
    write("MODULE.bazel", &root)?;
    write(".bazelversion", "9.3.2\n")?;
    write(
        ".bazelrc",
        "common --java_runtime_version=remotejdk_21 --tool_java_runtime_version=remotejdk_21\n",
    )?;
    write("BUILD.bazel", "")?;

    for m in 0..spec.modules {
        let name = module_name(m);
        let used: BTreeSet<usize> = targets
            .iter()
            .filter(|t| t.module == m)
            .flat_map(|t| t.deps.iter().map(|&(dm, _)| dm))
            .filter(|&dm| dm != m)
            .collect();
        let mut module = format!("module(name = \"{name}\")\n\n{rule_deps}");
        for dm in used {
            let _ = writeln!(module, "bazel_dep(name = \"{}\")", module_name(dm));
        }
        write(&format!("mods/{name}/MODULE.bazel"), &module)?;
    }

    for t in &targets {
        let (name, target) = (module_name(t.module), target_name(t.index));
        let deps: Vec<String> = t
            .deps
            .iter()
            .map(|&(dm, di)| {
                if dm == t.module {
                    format!("//{}", target_name(di))
                } else {
                    format!("@{}//{}", module_name(dm), target_name(di))
                }
            })
            .collect();
        let deps_attr = deps
            .iter()
            .map(|d| format!("\"{d}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!(
            "src/main/kotlin/synth/{name}/{target}/{}.kt",
            class_name(t.index)
        );
        write(
            &format!("mods/{name}/{target}/BUILD.bazel"),
            &format!(
                "load(\"@rules_kotlin//kotlin:jvm.bzl\", \"kt_jvm_library\")\n\nkt_jvm_library(\n    name = \"{target}\",\n    srcs = [\"{src}\"],\n    visibility = [\"//visibility:public\"],\n    deps = [{deps_attr}],\n)\n"
            ),
        )?;
        let calls: String = t
            .deps
            .iter()
            .map(|&(dm, di)| format!("{}().call() + ", fqcn(dm, di)))
            .collect();
        write(
            &format!("mods/{name}/{target}/{src}"),
            &format!(
                "package synth.{name}.{target}\n\nclass {} {{\n    fun call(): Int = {calls}1\n}}\n",
                class_name(t.index)
            ),
        )?;
    }

    let last = spec.targets_per_module.saturating_sub(1);
    let app_deps = (0..spec.modules)
        .map(|m| format!("\"@{}//{}\"", module_name(m), target_name(last)))
        .collect::<Vec<_>>()
        .join(", ");
    write(
        "app/BUILD.bazel",
        &format!(
            "load(\"@rules_kotlin//kotlin:jvm.bzl\", \"kt_jvm_library\")\n\nkt_jvm_library(\n    name = \"app\",\n    srcs = [\"src/main/kotlin/synth/app/App.kt\"],\n    deps = [{app_deps}],\n)\n"
        ),
    )?;
    let mut app = String::from("package synth.app\n\n");
    for m in 0..spec.modules {
        let _ = writeln!(app, "import {}", fqcn(m, last));
    }
    let calls = (0..spec.modules)
        .map(|_| format!("{}().call()", class_name(last)))
        .collect::<Vec<_>>()
        .join(" + ");
    let _ = write!(app, "\nfun main() = println({calls})\n");
    write("app/src/main/kotlin/synth/app/App.kt", &app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            modules: 20,
            targets_per_module: 100,
            seed: 1,
        }
    }

    #[test]
    fn generated_graph_is_acyclic_and_sized() {
        let targets = plan(&spec());
        assert_eq!(targets.len(), 2000);
        for t in &targets {
            for &(m, i) in &t.deps {
                assert!(
                    m < t.module || (m == t.module && i < t.index),
                    "{t:?} -> ({m}, {i})"
                );
                assert!(i < 100 && m < 20);
            }
        }
        assert!(
            targets
                .iter()
                .any(|t| t.deps.iter().any(|&(m, _)| m != t.module))
        );
    }

    #[test]
    fn rule_versions_match_fixture() {
        let fixture = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/basic/MODULE.bazel"
        ))
        .unwrap();
        for (name, version) in RULE_VERSIONS {
            assert!(
                fixture.contains(&format!(
                    "bazel_dep(name = \"{name}\", version = \"{version}\")"
                )),
                "{name} {version}"
            );
        }
    }

    #[test]
    fn generation_is_deterministic() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let small = Spec {
            modules: 3,
            targets_per_module: 4,
            seed: 7,
        };
        generate(&small, a.path()).unwrap();
        generate(&small, b.path()).unwrap();
        assert_eq!(snapshot(a.path()), snapshot(b.path()));
        assert!(a.path().join("mods/m02/t003/BUILD.bazel").is_file());
        assert!(a.path().join("app/BUILD.bazel").is_file());
    }

    fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push((
                        p.strip_prefix(root).unwrap().to_path_buf(),
                        std::fs::read(&p).unwrap(),
                    ));
                }
            }
        }
        out.sort();
        out
    }
}
