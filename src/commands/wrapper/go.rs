use acton_config::config::{ActonConfig, GoWrapperSettings, manifest_path, project_root};
use anyhow::Context;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::NamedTempFile;

const GO_WRAPPER_MODULE: &str =
    "github.com/ton-blockchain/tolk-abi-to-go/cmd/tolk-abi-to-go@v0.1.0";
// Tracks the `go` directive of the pinned generator module above.
const GO_VERSION: &str = "1.26.3";

pub fn go_wrapper_cmd(
    contract_id: Option<&str>,
    all: bool,
    catalog: Option<&Path>,
    output_dir: Option<&str>,
    package: Option<&str>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        usize::from(contract_id.is_some()) + usize::from(all) + usize::from(catalog.is_some()) == 1,
        "Select exactly one contract name, --all, or --catalog"
    );
    let config = if catalog.is_some() && !manifest_path().exists() {
        ActonConfig::default()
    } else {
        ActonConfig::load_manifest().context("Failed to load Acton.toml")?
    };
    let project_settings = config
        .wrappers
        .as_ref()
        .and_then(|wrappers| wrappers.go.as_ref());
    let contract_settings = contract_id
        .and_then(|id| config.get_contract(id))
        .and_then(|contract| contract.wrappers.as_ref())
        .and_then(|wrappers| wrappers.go.as_ref());
    let (output_dir, package) = resolve_settings(
        project_root(),
        project_settings,
        contract_settings,
        output_dir,
        package,
    );

    // Keep the file alive until Go exits, including on failure. One invocation for
    // the entire selection prevents contracts from replacing the shared registry.
    let mut temporary_catalog;
    let catalog = if let Some(catalog) = catalog {
        catalog
    } else {
        let catalog = super::compile_go_catalog(&config, contract_id)?;
        temporary_catalog =
            NamedTempFile::new().context("Failed to create temporary Go ABI catalog")?;
        serde_json::to_writer(&mut temporary_catalog, &catalog)
            .context("Failed to write temporary Go ABI catalog")?;
        temporary_catalog
            .flush()
            .context("Failed to flush temporary Go ABI catalog")?;
        temporary_catalog.path()
    };
    run_generator(catalog, &output_dir, &package)
}

fn resolve_settings(
    root: &Path,
    project: Option<&GoWrapperSettings>,
    contract: Option<&GoWrapperSettings>,
    output_dir: Option<&str>,
    package: Option<&str>,
) -> (PathBuf, String) {
    let setting = |field: fn(&GoWrapperSettings) -> Option<&str>| {
        contract
            .and_then(field)
            .filter(|s| !s.trim().is_empty())
            .or_else(|| project.and_then(field).filter(|s| !s.trim().is_empty()))
    };
    let output_dir = output_dir.map_or_else(
        || root.join(setting(|s| s.output_dir.as_deref()).unwrap_or("wrappers-go")),
        PathBuf::from,
    );
    let package = package
        .or_else(|| setting(|s| s.package.as_deref()))
        .unwrap_or("wrappers")
        .to_owned();
    (output_dir, package)
}

fn run_generator(catalog: &Path, output_dir: &Path, package: &str) -> anyhow::Result<()> {
    // CLI paths belong to the caller, not to the working directory below. Output may
    // not exist yet, so resolve lexically rather than canonicalizing on disk.
    let catalog = std::path::absolute(catalog).context("Failed to resolve catalog path")?;
    let output_dir =
        std::path::absolute(output_dir).context("Failed to resolve Go output directory")?;
    // `go run pkg@version` ignores the go.mod of the current directory and every
    // parent, so no module has to be unpacked. An empty directory is still used so
    // that nothing in a caller's workspace can influence the generator run.
    let workdir = tempfile::Builder::new()
        .prefix("acton-go-")
        .tempdir()
        .context("Failed to create temporary Go working directory")?;
    let status = Command::new("go")
        .args(["run", "-mod=readonly", "-trimpath", GO_WRAPPER_MODULE])
        .arg("--catalog").arg(&catalog)
        .arg("--output-dir").arg(&output_dir)
        .arg("--package").arg(package)
        .env("CGO_ENABLED", "0")
        .env("GOWORK", "off")
        // Go treats an empty GOFLAGS as unset and falls back to persistent GOENV
        // defaults. A nonempty whitespace value parses as no flags, preventing a
        // caller's -modfile or other build flags from changing this generator run.
        .env("GOFLAGS", " ")
        .current_dir(workdir.path())
        .status()
        .with_context(|| format!(
            "Failed to run Go for Acton's wrapper generator. Install Go {GO_VERSION} or newer from https://go.dev/dl/ and ensure `go` is on PATH. Go downloads {GO_WRAPPER_MODULE} itself on first use."
        ))?;
    anyhow::ensure!(
        status.success(),
        "Go wrapper generation failed with {status}. See Go output above for details. The generator requires Go {GO_VERSION} or newer (https://go.dev/dl/)."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_settings_precedence_and_path_resolution() {
        let root = Path::new("/project");
        assert_eq!(
            resolve_settings(root, None, None, None, None),
            (root.join("wrappers-go"), "wrappers".into())
        );
        let project = GoWrapperSettings {
            output_dir: Some("generated go".into()),
            package: Some("codecs".into()),
        };
        let contract = GoWrapperSettings {
            package: Some("counter".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_settings(root, Some(&project), Some(&contract), None, None),
            (root.join("generated go"), "counter".into())
        );
        assert_eq!(
            resolve_settings(
                root,
                Some(&project),
                Some(&contract),
                Some("cwd/go"),
                Some("override")
            ),
            ("cwd/go".into(), "override".into())
        );
    }
}
