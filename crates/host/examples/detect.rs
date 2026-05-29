//! Smoke-test the install detector against the real machine.
//!
//! `cargo run -p ncp-host --example detect`
//!
//! Prints every UT4 *play* install found (desktop-shortcut driven, with a
//! directory-probe fallback), how it was found, and whether NetcodePlus is
//! correctly installed in it. Editor/source trees should NOT appear.

fn main() {
    let installs = ncp_host::detect_installs();
    if installs.is_empty() {
        println!("No UT4 play install detected (no shortcut and no probe hit).");
        return;
    }
    println!("Detected {} play install(s):\n", installs.len());
    for di in installs {
        println!("  root        : {}", di.install.root.display());
        println!("  source      : {:?}", di.source);
        println!("  NetcodePlus : {:?}", di.netcodeplus);
        println!("  executable  : {}", di.install.executable.display());
        println!("  launch args : {}", di.install.launch_args.join(" "));
        println!();
    }
}
