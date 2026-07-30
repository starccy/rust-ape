//! Prints everything the `ape` crate exposes. The same binary prints
//! different results on each host.

fn main() {
    println!("os\t\t: {:?}", ape::current_os());
    println!(
        "linux/win/xnu/bsd/metal\t: {} {} {} {} {}",
        ape::is_linux(),
        ape::is_windows(),
        ape::is_xnu(),
        ape::is_bsd(),
        ape::is_metal()
    );
    println!("qemu-user\t: {}", ape::is_qemu_user());
    println!("cpu count\t: {}", ape::cpu_count());
    println!("page size\t: {}", ape::page_size());
    println!(
        "physical memory\t: {} pages, {} MiB",
        ape::phys_pages(),
        ape::total_ram_bytes() / 1024 / 1024
    );
    println!(
        "available\t: {} pages, {} MiB",
        ape::avail_phys_pages(),
        ape::avail_ram_bytes() / 1024 / 1024
    );
    println!("program\t\t: {:?}", ape::program_executable_name());
    println!("interpreter\t: {:?}", ape::interpreter_executable_name());
    println!("tmpdir\t\t: {:?}", ape::tmpdir());
    println!("auxval AT_HWCAP\t: {:#x}", ape::auxval(16));

    #[cfg(target_arch = "x86_64")]
    {
        println!("cpuid vendor\t: {}", ape::cpuid_vendor());
        println!("cpuid os\t: {:?}", ape::cpuid_os());
        println!("cpuid emulator\t: {:?}", ape::cpuid_emulator());
    }
}
