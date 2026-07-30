use crate::config;
use crate::fetch;
use crate::util;
use anyhow::{Context, Result, bail};
use clap::Args;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[derive(Args)]
pub struct SetupArgs {
    /// Ignore stamps and rebuild everything under vendor/
    #[arg(long)]
    pub force: bool,
}

/// setup = restore vendor/ + materialize.
pub fn run(args: &SetupArgs) -> Result<()> {
    fetch::run(args.force)?;
    materialize()?;
    println!("setup done");
    Ok(())
}

/// Renders the targets/ templates into a generated/ that works on this machine:
/// a linker shim per arch (sets ARCH, execs the wrapper), and a target JSON with
/// @APE_LINKER@ pointing at it.
///
/// Cheap enough that there's no stamp; rewriting on every setup keeps it in
/// sync with the templates and the current repo location.
fn materialize() -> Result<()> {
    let root = util::repo_root();
    let generated = root.join("generated");
    fs::create_dir_all(&generated)?;

    let wrapper = root.join("scripts/gcc-linker-wrapper.bash");
    if !wrapper.is_file() {
        bail!("missing {}", wrapper.display());
    }

    for &(triple, arch) in config::TARGETS {
        let shim = generated.join(format!("linker-{arch}.bash"));
        fs::write(
            &shim,
            format!(
                "#!/usr/bin/env bash\n# Written by `cargo xtask setup`; edits will be overwritten.\nexport ARCH={arch}\nexec \"{}\" \"$@\"\n",
                wrapper.display()
            ),
        )?;
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755))?;

        // cosmo's ar is an APE binary; cc-rs execs it directly and gets ENOEXEC
        // wherever binfmt_misc isn't registered, so route it through sh.
        let ar = root
            .join("vendor/cosmocc/bin")
            .join(format!("{arch}-unknown-cosmo-ar"));
        let ar_shim = generated.join(format!("ar-{arch}.bash"));
        fs::write(
            &ar_shim,
            format!(
                "#!/usr/bin/env bash\n# Written by `cargo xtask setup`; edits will be overwritten.\nexec sh \"{}\" \"$@\"\n",
                ar.display()
            ),
        )?;
        fs::set_permissions(&ar_shim, fs::Permissions::from_mode(0o755))?;

        let template = root.join("targets").join(format!("{triple}.json"));
        let json = fs::read_to_string(&template)
            .with_context(|| format!("could not read template {}", template.display()))?;
        if !json.contains("@APE_LINKER@") {
            bail!("template {} has no @APE_LINKER@ placeholder", template.display());
        }
        let rendered = json.replace("@APE_LINKER@", &shim.display().to_string());
        let out = generated.join(format!("{triple}.json"));
        fs::write(&out, rendered)?;
        println!("==> wrote generated/{triple}.json");
    }
    Ok(())
}
