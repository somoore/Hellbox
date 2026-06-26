//! Build the capsule image and record its image ARN.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use aws_sdk_lambdamicrovms::types::{
    CodeArtifact, HookState, Hooks, MicrovmHooks, MicrovmImageHooks,
};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;

use crate::aws::Aws;
use crate::config::Config;
use crate::poll::{PollOpts, poll_until};
use crate::state::State;

/// AWS probes lifecycle hooks on this port.
const HOOK_PORT: i32 = 9000;

pub async fn run(name: &str, app: Option<&str>, capsule_dir_override: Option<&str>) -> Result<()> {
    let cfg = Config::load()?;
    let mut state = State::load()?;

    let capsule_dir = capsule_dir(capsule_dir_override)?;
    if let Some(app) = app {
        tracing::info!(target: "hellbox::build", "note: --app {app} — ensure it's staged under capsule/app/");
    }

    let zip_path = zip_context(&capsule_dir)
        .with_context(|| format!("zipping build context at {}", capsule_dir.display()))?;
    tracing::info!(target: "hellbox::build", "built context zip at {}", zip_path.display());

    let key = format!("contexts/{name}.zip");
    let bytes = std::fs::read(&zip_path).context("reading context zip")?;
    let aws = Aws::new(&cfg).await?;
    aws.s3
        .put_object()
        .bucket(&cfg.artifact_bucket)
        .key(&key)
        .body(bytes.into())
        .send()
        .await
        .with_context(|| format!("uploading s3://{}/{key}", cfg.artifact_bucket))?;
    let code_artifact_uri = format!("s3://{}/{}", cfg.artifact_bucket, key);
    tracing::info!(target: "hellbox::build", "uploaded {code_artifact_uri}");

    let hooks = Hooks::builder()
        .port(HOOK_PORT)
        .microvm_image_hooks(
            MicrovmImageHooks::builder()
                .ready(HookState::Enabled)
                .ready_timeout_in_seconds(600)
                .validate(HookState::Disabled)
                .validate_timeout_in_seconds(60)
                .build(),
        )
        .microvm_hooks(
            MicrovmHooks::builder()
                .run(HookState::Enabled)
                .run_timeout_in_seconds(60)
                .resume(HookState::Enabled)
                .resume_timeout_in_seconds(60)
                .suspend(HookState::Disabled)
                .suspend_timeout_in_seconds(60)
                .terminate(HookState::Disabled)
                .terminate_timeout_in_seconds(60)
                .build(),
        )
        .build();
    let created = aws
        .microvm
        .create_microvm_image()
        .name(name)
        .base_image_arn(&cfg.base_image_arn)
        .build_role_arn(&cfg.build_role_arn)
        .code_artifact(CodeArtifact::Uri(code_artifact_uri))
        .hooks(hooks)
        .client_token(client_token(name))
        .send()
        .await
        .context("create_microvm_image")?;
    let image_arn = created.image_arn().to_string();
    tracing::info!(target: "hellbox::build", "image creating: {image_arn} (state {})", created.state().as_str());

    state.upsert(name, |c| {
        c.image_arn = Some(image_arn.clone());
        c.image_version = created.latest_active_image_version().map(str::to_string);
        c.state = Some(created.state().as_str().to_string());
    })?;

    let image_id = image_arn.clone();
    let final_state = poll_until(
        &format!("image {name}"),
        &["CREATED", "CREATE_FAILED"],
        PollOpts::default(),
        || {
            let aws = &aws;
            let image_id = image_id.clone();
            async move {
                let out = aws
                    .microvm
                    .get_microvm_image()
                    .image_identifier(&image_id)
                    .send()
                    .await
                    .context("get_microvm_image")?;
                Ok(out.state().as_str().to_string())
            }
        },
    )
    .await?;

    let active_version = aws
        .microvm
        .get_microvm_image()
        .image_identifier(&image_arn)
        .send()
        .await
        .ok()
        .and_then(|o| o.latest_active_image_version().map(str::to_string));

    state.upsert(name, |c| {
        c.state = Some(final_state.clone());
        if active_version.is_some() {
            c.image_version = active_version.clone();
        }
    })?;

    if final_state == "CREATE_FAILED" {
        bail!("image build for '{name}' failed (state CREATE_FAILED)");
    }

    println!("built '{name}': image {image_arn} CREATED");
    Ok(())
}

fn capsule_dir(override_path: Option<&str>) -> Result<PathBuf> {
    let dir = match override_path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir()?.join("capsule"),
    };
    if !dir.is_dir() {
        bail!(
            "no capsule dir at {} — run `hellbox build` from the Hellbox repo root, \
             or pass --capsule-dir <PATH>",
            dir.display()
        );
    }
    Ok(dir)
}

fn client_token(name: &str) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("hellbox-build-{name}-{secs}")
}

fn zip_context(dir: &Path) -> Result<PathBuf> {
    let out_path = std::env::temp_dir().join(format!("hellbox-context-{}.zip", std::process::id()));
    let file = std::fs::File::create(&out_path)
        .with_context(|| format!("creating {}", out_path.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o755);

    // follow_links(false) (the default) yields symlinks as symlink entries rather
    // than recursing/following them, so path_is_symlink() can catch them below.
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        let rel = path.strip_prefix(dir).context("relativizing zip entry")?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        // SECURITY: fail closed on symlinks. std::fs::read() follows a file symlink,
        // so a symlink inside the capsule (e.g. -> ~/.aws/credentials) would package
        // the TARGET's bytes into the zip and upload them to S3 in the build context.
        // The capsule legitimately contains none, so reject rather than follow.
        if entry.path_is_symlink() {
            bail!(
                "refusing to build: capsule contains a symlink ({}). Symlinks are not \
                 packaged (their targets could exfiltrate local files into the cloud \
                 build context). Remove it or replace it with a real file.",
                rel.display()
            );
        }
        let name = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");

        if path.is_dir() {
            zip.add_directory(format!("{name}/"), opts)
                .with_context(|| format!("adding dir {name}"))?;
        } else {
            zip.start_file(&name, opts)
                .with_context(|| format!("adding file {name}"))?;
            let data =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            zip.write_all(&data)
                .with_context(|| format!("writing {name} into zip"))?;
        }
    }
    zip.finish().context("finalizing zip")?;
    Ok(out_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unique scratch dir under the system temp, cleaned up on drop. Avoids a
    // tempfile dev-dependency for one test.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "hellbox-buildtest-{tag}-{}-{:p}",
                std::process::id(),
                &tag as *const _
            ));
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn zip_context_packages_normal_files() {
        let s = Scratch::new("normal");
        std::fs::write(s.0.join("a.txt"), b"hello").unwrap();
        std::fs::create_dir(s.0.join("sub")).unwrap();
        std::fs::write(s.0.join("sub/b.txt"), b"world").unwrap();
        let zip = zip_context(&s.0).expect("normal capsule should package");
        assert!(zip.exists(), "zip produced");
        let _ = std::fs::remove_file(zip);
    }

    #[cfg(unix)]
    #[test]
    fn zip_context_rejects_symlinks() {
        let s = Scratch::new("symlink");
        // A secret outside the capsule the symlink would exfiltrate.
        let secret = s.0.join("outside_secret");
        std::fs::write(&secret, b"AWS_SECRET").unwrap();
        let cap = s.0.join("capsule");
        std::fs::create_dir(&cap).unwrap();
        std::fs::write(cap.join("real.txt"), b"ok").unwrap();
        std::os::unix::fs::symlink(&secret, cap.join("link.txt")).unwrap();

        let err = zip_context(&cap).expect_err("symlink must be rejected");
        assert!(
            err.to_string().contains("symlink"),
            "error names the symlink: {err}"
        );
    }
}
