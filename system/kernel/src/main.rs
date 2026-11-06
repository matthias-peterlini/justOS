//! The JustOS kernel.
//!
//! Boot order: serial console → CPU tables → memory → screen (and the boot
//! splash) → clock and scheduler → interrupts on → ACPI → boot pack and RAM
//! volume → device scan → boot drivers (the system disk mounts) → every
//! other driver → graphical interface and shell.

#![no_std]
#![no_main]

extern crate alloc;

mod acpi;
mod appupdate;
mod arch;
mod boot;
mod console;
mod dev;
mod driver;
mod fs;
mod gui;
mod ipc;
mod linux;
mod mem;
mod net;
mod rand;
mod sandbox;
mod shell;
mod sleep;
mod store;
mod sync;
mod sysupdate;
mod task;
mod time;
mod update;
mod user;
mod util;

use arch::{cpu, gdt, idt, pic, pit};
use boot::BootInfo;
use core::arch::global_asm;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

global_asm!(
    r#"
    .section .text.boot, "ax"
    .global _start
_start:
    cli
    mov r12, rdi
    lea rdi, [rip + __bss_start]
    lea rcx, [rip + __bss_end]
    sub rcx, rdi
    xor eax, eax
    rep stosb
    lea rsp, [rip + {stack} + {stack_size}]
    xor ebp, ebp
    mov rdi, r12
    call kmain
2:
    cli
    hlt
    jmp 2b
"#,
    stack = sym BOOT_STACK,
    stack_size = const BOOT_STACK_SIZE,
);

const BOOT_STACK_SIZE: usize = 64 * 1024;

#[repr(C, align(16))]
struct Stack([u8; BOOT_STACK_SIZE]);

#[no_mangle]
static mut BOOT_STACK: Stack = Stack([0; BOOT_STACK_SIZE]);

#[no_mangle]
extern "C" fn kmain(boot_phys: u64) -> ! {
    console::serial::init();
    console::early("\nJustOS kernel ");
    console::early(VERSION);
    console::early("\n");

    // The loader's identity map is still active: read the hand-off now.
    // SAFETY: the loader passes the physical address of a BootInfo.
    let boot: BootInfo = unsafe { core::ptr::read(boot_phys as *const BootInfo) };
    if boot.magic != boot::MAGIC {
        console::early("bad boot information from the loader\n");
        cpu::halt_forever();
    }
    sysupdate::init(boot.slot);

    cpu::enable_sse();
    gdt::init();
    arch::smp::init_bsp();
    idt::init();
    arch::syscall::init();
    pic::init();
    pit::init();
    mem::init(&boot);
    console::init_screen(&boot);
    gui::boot_start();
    time::init();
    task::init();
    gui::boot_progress(8);
    shell::banner(&boot);

    dev::irq::attach_kernel(4, console::serial::irq_handler);
    console::serial::enable_rx_irq();
    pic::unmask(0);
    cpu::enable_irq();

    let (free, usable) = mem::frame::stats();
    info!("kernel", "{} of memory, {} free", util::Size(usable), util::Size(free));
    acpi::init(boot.rsdp, boot.memory_map());
    arch::smp::start();
    gui::boot_progress(18);

    fs::mount_boot_pack(boot.pack_phys, boot.pack_size);
    if boot.system_size != 0 {
        fs::mount_live_system(boot.system_phys, boot.system_size);
    }
    fs::mount_ram(4 << 20);
    gui::boot_progress(28);

    let found = dev::scan().len();
    acpi::start_events();
    debug!("kernel", "{found} devices found");
    gui::boot_progress(38);
    driver::init();
    gui::boot_progress(60);
    if fs::volume("sys").is_none()
        && dev::list().iter().any(|d| d.driver.is_some() && d.info.bus == justdk::BUS_PCI && d.info.class == 0x0C && d.info.subclass == 0x03)
    {
        // The system disk may be a USB drive: its controller finds it in
        // the background.
        info!("kernel", "waiting for USB disks");
        driver::wait_for_system(8000);
    }
    if fs::volume("sys").is_none() {
        warn!("kernel", "no system volume found: only boot: and ram: are available");
    }
    driver::configure_all();
    gui::boot_progress(75);
    driver::process_inbox();
    driver::start_monitor();
    net::start();
    ipc::start_services();
    appupdate::start();
    gui::boot_progress(82);

    gui::start();
    sysupdate::start();
    if gui::active() {
        update::check_when_online();
    }
    shell::run();
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    console::panic_screen(format_args!("kernel panic: {info}"));
}
