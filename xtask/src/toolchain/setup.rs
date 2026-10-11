use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::toolchain::config;
use crate::toolchain::fetch;
use crate::util;

/// What the command was asked to do; cli/setup.rs has the flags.
pub struct Options {
    /// Ignore stamps and rebuild everything under vendor/
    pub force: bool,
    /// Use a locally built toolchain instead of the pinned release zip
    pub cosmocc: Option<PathBuf>,
}

/// setup = restore vendor/ + materialize.
pub fn run(args: &Options) -> Result<()> {
    fetch::run(args.force, args.cosmocc.as_deref())?;
    materialize()?;
    println!("setup done");
    Ok(())
}

/// Renders the targets/ templates into a generated/ that works on this machine:
/// a linker shim per arch (sets ARCH, execs the wrapper), and a target JSON with
/// @APE_LINKER@ pointing at it.
fn materialize() -> Result<()> {
    let root = util::repo_root();
    let generated = root.join("generated");
    fs::create_dir_all(&generated)?;

    let wrapper = root.join("scripts/gcc-linker-wrapper.bash");
    let cc_wrapper = root.join("scripts/cc-wrapper.bash");
    for w in [&wrapper, &cc_wrapper] {
        if !w.is_file() {
            bail!("missing {}", w.display());
        }
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
            .join(format!("{arch}-linux-cosmo-ar"));
        let ar_shim = generated.join(format!("ar-{arch}.bash"));
        fs::write(
            &ar_shim,
            format!(
                "#!/usr/bin/env bash\n# Written by `cargo xtask setup`; edits will be overwritten.\nexec sh \"{}\" \"$@\"\n",
                ar.display()
            ),
        )?;
        fs::set_permissions(&ar_shim, fs::Permissions::from_mode(0o755))?;

        let cc_shim = generated.join(format!("cc-{arch}.bash"));
        fs::write(
            &cc_shim,
            format!(
                "#!/usr/bin/env bash\n# Written by `cargo xtask setup`; edits will be overwritten.\nexport ARCH={arch}\nexec \"{}\" \"$@\"\n",
                cc_wrapper.display()
            ),
        )?;
        fs::set_permissions(&cc_shim, fs::Permissions::from_mode(0o755))?;

        let template = root.join("targets").join(format!("{triple}.json"));
        let json = fs::read_to_string(&template)
            .with_context(|| format!("could not read template {}", template.display()))?;
        if !json.contains("@APE_LINKER@") {
            bail!(
                "template {} has no @APE_LINKER@ placeholder",
                template.display()
            );
        }
        let rendered = json.replace("@APE_LINKER@", &shim.display().to_string());
        let out = generated.join(format!("{triple}.json"));
        fs::write(&out, rendered)?;
        println!("==> wrote generated/{triple}.json");
    }
    Ok(())
}
