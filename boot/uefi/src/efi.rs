//! The parts of the UEFI interface the loader uses, declared by hand from
//! the specification (2.10). Every call uses the Microsoft x64 convention.

use core::ffi::c_void;

pub type Handle = *mut c_void;
pub type Status = usize;

pub const SUCCESS: Status = 0;
pub const ERROR_BIT: Status = 1 << 63;
pub const BUFFER_TOO_SMALL: Status = ERROR_BIT | 5;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

pub const LOADED_IMAGE_PROTOCOL: Guid = Guid(0x5B1B31A1, 0x9562, 0x11D2, [0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B]);
pub const SIMPLE_FILE_SYSTEM_PROTOCOL: Guid = Guid(0x964E5B22, 0x6459, 0x11D2, [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B]);
pub const GRAPHICS_OUTPUT_PROTOCOL: Guid = Guid(0x9042A9DE, 0x23DC, 0x4A38, [0x96, 0xFB, 0x7A, 0xDE, 0xD0, 0x80, 0x51, 0x6A]);
pub const ACPI_20_TABLE: Guid = Guid(0x8868E871, 0xE4F1, 0x11D3, [0xBC, 0x22, 0x00, 0x80, 0xC7, 0x3C, 0x88, 0x81]);
pub const ACPI_10_TABLE: Guid = Guid(0xEB9D2D30, 0x2D88, 0x11D3, [0x9A, 0x16, 0x00, 0x90, 0x27, 0x3F, 0xC1, 0x4D]);

#[repr(C)]
pub struct TableHeader {
    pub signature: u64,
    pub revision: u32,
    pub header_size: u32,
    pub crc32: u32,
    pub reserved: u32,
}

#[repr(C)]
pub struct SystemTable {
    pub hdr: TableHeader,
    pub firmware_vendor: *const u16,
    pub firmware_revision: u32,
    pub console_in_handle: Handle,
    pub con_in: *mut c_void,
    pub console_out_handle: Handle,
    pub con_out: *mut TextOutput,
    pub standard_error_handle: Handle,
    pub std_err: *mut TextOutput,
    pub runtime_services: *mut c_void,
    pub boot_services: *mut BootServices,
    pub number_of_table_entries: usize,
    pub configuration_table: *const ConfigurationTable,
}

#[repr(C)]
pub struct ConfigurationTable {
    pub vendor_guid: Guid,
    pub vendor_table: *mut c_void,
}

#[repr(C)]
pub struct TextOutput {
    pub reset: unsafe extern "efiapi" fn(*mut TextOutput, bool) -> Status,
    pub output_string: unsafe extern "efiapi" fn(*mut TextOutput, *const u16) -> Status,
    pub test_string: usize,
    pub query_mode: usize,
    pub set_mode: usize,
    pub set_attribute: usize,
    pub clear_screen: unsafe extern "efiapi" fn(*mut TextOutput) -> Status,
    pub set_cursor_position: usize,
    pub enable_cursor: unsafe extern "efiapi" fn(*mut TextOutput, bool) -> Status,
}

// Memory allocation (AllocatePages type, memory types).
pub const ALLOCATE_ANY_PAGES: u32 = 0;
pub const ALLOCATE_MAX_ADDRESS: u32 = 1;

pub const RESERVED_MEMORY: u32 = 0;
pub const LOADER_CODE: u32 = 1;
pub const LOADER_DATA: u32 = 2;
pub const BOOT_SERVICES_CODE: u32 = 3;
pub const BOOT_SERVICES_DATA: u32 = 4;
pub const RUNTIME_SERVICES_CODE: u32 = 5;
pub const RUNTIME_SERVICES_DATA: u32 = 6;
pub const CONVENTIONAL_MEMORY: u32 = 7;
pub const UNUSABLE_MEMORY: u32 = 8;
pub const ACPI_RECLAIM_MEMORY: u32 = 9;
pub const ACPI_MEMORY_NVS: u32 = 10;
pub const MEMORY_MAPPED_IO: u32 = 11;
pub const MEMORY_MAPPED_IO_PORT_SPACE: u32 = 12;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemoryDescriptor {
    pub kind: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

type Unused = usize;

#[repr(C)]
pub struct BootServices {
    pub hdr: TableHeader,
    pub raise_tpl: Unused,
    pub restore_tpl: Unused,
    pub allocate_pages: unsafe extern "efiapi" fn(u32, u32, usize, *mut u64) -> Status,
    pub free_pages: unsafe extern "efiapi" fn(u64, usize) -> Status,
    pub get_memory_map: unsafe extern "efiapi" fn(*mut usize, *mut u8, *mut usize, *mut usize, *mut u32) -> Status,
    pub allocate_pool: unsafe extern "efiapi" fn(u32, usize, *mut *mut u8) -> Status,
    pub free_pool: unsafe extern "efiapi" fn(*mut u8) -> Status,
    pub create_event: Unused,
    pub set_timer: Unused,
    pub wait_for_event: Unused,
    pub signal_event: Unused,
    pub close_event: Unused,
    pub check_event: Unused,
    pub install_protocol_interface: Unused,
    pub reinstall_protocol_interface: Unused,
    pub uninstall_protocol_interface: Unused,
    pub handle_protocol: unsafe extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status,
    pub reserved: Unused,
    pub register_protocol_notify: Unused,
    pub locate_handle: Unused,
    pub locate_device_path: Unused,
    pub install_configuration_table: Unused,
    pub load_image: Unused,
    pub start_image: Unused,
    pub exit: Unused,
    pub unload_image: Unused,
    pub exit_boot_services: unsafe extern "efiapi" fn(Handle, usize) -> Status,
    pub get_next_monotonic_count: Unused,
    pub stall: unsafe extern "efiapi" fn(usize) -> Status,
    pub set_watchdog_timer: unsafe extern "efiapi" fn(usize, u64, usize, *const u16) -> Status,
    pub connect_controller: Unused,
    pub disconnect_controller: Unused,
    pub open_protocol: Unused,
    pub close_protocol: Unused,
    pub open_protocol_information: Unused,
    pub protocols_per_handle: Unused,
    pub locate_handle_buffer: Unused,
    pub locate_protocol: unsafe extern "efiapi" fn(*const Guid, *mut c_void, *mut *mut c_void) -> Status,
}

#[repr(C)]
pub struct LoadedImage {
    pub revision: u32,
    pub parent_handle: Handle,
    pub system_table: *mut SystemTable,
    pub device_handle: Handle,
    // The rest is not needed.
}

#[repr(C)]
pub struct SimpleFileSystem {
    pub revision: u64,
    pub open_volume: unsafe extern "efiapi" fn(*mut SimpleFileSystem, *mut *mut File) -> Status,
}

pub const FILE_MODE_READ: u64 = 1;
pub const FILE_MODE_WRITE: u64 = 2;

#[repr(C)]
pub struct File {
    pub revision: u64,
    pub open: unsafe extern "efiapi" fn(*mut File, *mut *mut File, *const u16, u64, u64) -> Status,
    pub close: unsafe extern "efiapi" fn(*mut File) -> Status,
    pub delete: Unused,
    pub read: unsafe extern "efiapi" fn(*mut File, *mut usize, *mut u8) -> Status,
    pub write: unsafe extern "efiapi" fn(*mut File, *mut usize, *const u8) -> Status,
    pub get_position: unsafe extern "efiapi" fn(*mut File, *mut u64) -> Status,
    pub set_position: unsafe extern "efiapi" fn(*mut File, u64) -> Status,
    pub get_info: Unused,
    pub set_info: Unused,
    pub flush: unsafe extern "efiapi" fn(*mut File) -> Status,
}

pub const PIXEL_RGB: u32 = 0;
pub const PIXEL_BGR: u32 = 1;
pub const PIXEL_BIT_MASK: u32 = 2;

#[repr(C)]
pub struct GraphicsOutput {
    pub query_mode: unsafe extern "efiapi" fn(*mut GraphicsOutput, u32, *mut usize, *mut *const ModeInfo) -> Status,
    pub set_mode: unsafe extern "efiapi" fn(*mut GraphicsOutput, u32) -> Status,
    pub blt: Unused,
    pub mode: *const GraphicsMode,
}

#[repr(C)]
pub struct GraphicsMode {
    pub max_mode: u32,
    pub mode: u32,
    pub info: *const ModeInfo,
    pub size_of_info: usize,
    pub frame_buffer_base: u64,
    pub frame_buffer_size: usize,
}

#[repr(C)]
pub struct ModeInfo {
    pub version: u32,
    pub width: u32,
    pub height: u32,
    pub pixel_format: u32,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    pub reserved_mask: u32,
    pub pixels_per_scan_line: u32,
}
