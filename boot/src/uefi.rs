//! Minimal hand-written UEFI bindings: only the tables and protocols the boot
//! manager actually uses. Layouts follow the UEFI 2.10 specification. Unused
//! function pointers are declared as `usize` so the struct offsets stay right.

#![allow(dead_code)]

use core::ffi::c_void;

pub type Handle = *mut c_void;
pub type Status = usize;

const ERROR_BIT: usize = 1 << 63;
pub const SUCCESS: Status = 0;
pub const LOAD_ERROR: Status = ERROR_BIT | 1;
pub const INVALID_PARAMETER: Status = ERROR_BIT | 2;
pub const UNSUPPORTED: Status = ERROR_BIT | 3;
pub const BUFFER_TOO_SMALL: Status = ERROR_BIT | 5;
pub const OUT_OF_RESOURCES: Status = ERROR_BIT | 9;
pub const NOT_FOUND: Status = ERROR_BIT | 14;

pub fn is_error(status: Status) -> bool {
    status & ERROR_BIT != 0
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid {
    pub a: u32,
    pub b: u16,
    pub c: u16,
    pub d: [u8; 8],
}

pub const LOADED_IMAGE_GUID: Guid =
    Guid { a: 0x5B1B_31A1, b: 0x9562, c: 0x11D2, d: [0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B] };
pub const SIMPLE_FILE_SYSTEM_GUID: Guid =
    Guid { a: 0x964E_5B22, b: 0x6459, c: 0x11D2, d: [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B] };
pub const FILE_INFO_GUID: Guid =
    Guid { a: 0x0957_6E92, b: 0x6D3F, c: 0x11D2, d: [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B] };
pub const GRAPHICS_OUTPUT_GUID: Guid =
    Guid { a: 0x9042_A9DE, b: 0x23DC, c: 0x4A38, d: [0x96, 0xFB, 0x7A, 0xDE, 0xD0, 0x80, 0x51, 0x6A] };
pub const ACPI_20_TABLE_GUID: Guid =
    Guid { a: 0x8868_E871, b: 0xE4F1, c: 0x11D3, d: [0xBC, 0x22, 0x00, 0x80, 0xC7, 0xC7, 0x3C, 0x81] };
pub const ACPI_10_TABLE_GUID: Guid =
    Guid { a: 0xEB9D_2D30, b: 0x2D88, c: 0x11D3, d: [0x9A, 0x16, 0x00, 0x90, 0x27, 0x3F, 0xC1, 0x4D] };

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
    pub con_out: *mut SimpleTextOutput,
    pub standard_error_handle: Handle,
    pub std_err: *mut SimpleTextOutput,
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
pub struct SimpleTextOutput {
    pub reset: unsafe extern "efiapi" fn(*mut SimpleTextOutput, u8) -> Status,
    pub output_string: unsafe extern "efiapi" fn(*mut SimpleTextOutput, *const u16) -> Status,
    pub test_string: usize,
    pub query_mode: usize,
    pub set_mode: usize,
    pub set_attribute: unsafe extern "efiapi" fn(*mut SimpleTextOutput, usize) -> Status,
    pub clear_screen: unsafe extern "efiapi" fn(*mut SimpleTextOutput) -> Status,
    pub set_cursor_position: usize,
    pub enable_cursor: usize,
    pub mode: *mut c_void,
}

pub const ALLOCATE_ANY_PAGES: u32 = 0;
pub const ALLOCATE_MAX_ADDRESS: u32 = 1;

pub const LOADER_DATA: u32 = 2;

#[repr(C)]
pub struct BootServices {
    pub hdr: TableHeader,
    pub raise_tpl: usize,
    pub restore_tpl: usize,
    pub allocate_pages: unsafe extern "efiapi" fn(u32, u32, usize, *mut u64) -> Status,
    pub free_pages: unsafe extern "efiapi" fn(u64, usize) -> Status,
    pub get_memory_map: unsafe extern "efiapi" fn(*mut usize, *mut u8, *mut usize, *mut usize, *mut u32) -> Status,
    pub allocate_pool: unsafe extern "efiapi" fn(u32, usize, *mut *mut u8) -> Status,
    pub free_pool: unsafe extern "efiapi" fn(*mut u8) -> Status,
    pub create_event: usize,
    pub set_timer: usize,
    pub wait_for_event: usize,
    pub signal_event: usize,
    pub close_event: usize,
    pub check_event: usize,
    pub install_protocol_interface: usize,
    pub reinstall_protocol_interface: usize,
    pub uninstall_protocol_interface: usize,
    pub handle_protocol: unsafe extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status,
    pub reserved: usize,
    pub register_protocol_notify: usize,
    pub locate_handle: usize,
    pub locate_device_path: usize,
    pub install_configuration_table: usize,
    pub load_image: usize,
    pub start_image: usize,
    pub exit: usize,
    pub unload_image: usize,
    pub exit_boot_services: unsafe extern "efiapi" fn(Handle, usize) -> Status,
    pub get_next_monotonic_count: usize,
    pub stall: unsafe extern "efiapi" fn(usize) -> Status,
    pub set_watchdog_timer: unsafe extern "efiapi" fn(usize, u64, usize, *const u16) -> Status,
    pub connect_controller: usize,
    pub disconnect_controller: usize,
    pub open_protocol: usize,
    pub close_protocol: usize,
    pub open_protocol_information: usize,
    pub protocols_per_handle: usize,
    pub locate_handle_buffer: usize,
    pub locate_protocol: unsafe extern "efiapi" fn(*const Guid, *mut c_void, *mut *mut c_void) -> Status,
}

/// UEFI memory types (UEFI spec table 7-6).
pub mod memory_type {
    pub const RESERVED: u32 = 0;
    pub const LOADER_CODE: u32 = 1;
    pub const LOADER_DATA: u32 = 2;
    pub const BOOT_SERVICES_CODE: u32 = 3;
    pub const BOOT_SERVICES_DATA: u32 = 4;
    pub const RUNTIME_SERVICES_CODE: u32 = 5;
    pub const RUNTIME_SERVICES_DATA: u32 = 6;
    pub const CONVENTIONAL: u32 = 7;
    pub const UNUSABLE: u32 = 8;
    pub const ACPI_RECLAIM: u32 = 9;
    pub const ACPI_NVS: u32 = 10;
    pub const MMIO: u32 = 11;
    pub const MMIO_PORT_SPACE: u32 = 12;
    pub const PAL_CODE: u32 = 13;
    pub const PERSISTENT: u32 = 14;
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemoryDescriptor {
    pub ty: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

#[repr(C)]
pub struct LoadedImage {
    pub revision: u32,
    pub parent_handle: Handle,
    pub system_table: *mut SystemTable,
    pub device_handle: Handle,
    pub file_path: *mut c_void,
    pub reserved: *mut c_void,
    pub load_options_size: u32,
    pub load_options: *mut c_void,
    pub image_base: *mut c_void,
    pub image_size: u64,
    pub image_code_type: u32,
    pub image_data_type: u32,
    pub unload: usize,
}

#[repr(C)]
pub struct SimpleFileSystem {
    pub revision: u64,
    pub open_volume: unsafe extern "efiapi" fn(*mut SimpleFileSystem, *mut *mut File) -> Status,
}

pub const FILE_MODE_READ: u64 = 1;

#[repr(C)]
pub struct File {
    pub revision: u64,
    pub open: unsafe extern "efiapi" fn(*mut File, *mut *mut File, *const u16, u64, u64) -> Status,
    pub close: unsafe extern "efiapi" fn(*mut File) -> Status,
    pub delete: usize,
    pub read: unsafe extern "efiapi" fn(*mut File, *mut usize, *mut u8) -> Status,
    pub write: usize,
    pub get_position: usize,
    pub set_position: usize,
    pub get_info: unsafe extern "efiapi" fn(*mut File, *const Guid, *mut usize, *mut u8) -> Status,
    pub set_info: usize,
    pub flush: usize,
}

pub const PIXEL_RGB_RESERVED_8BIT: u32 = 0;
pub const PIXEL_BGR_RESERVED_8BIT: u32 = 1;

#[repr(C)]
pub struct GraphicsOutput {
    pub query_mode: unsafe extern "efiapi" fn(*mut GraphicsOutput, u32, *mut usize, *mut *mut GopModeInfo) -> Status,
    pub set_mode: unsafe extern "efiapi" fn(*mut GraphicsOutput, u32) -> Status,
    pub blt: usize,
    pub mode: *mut GopMode,
}

#[repr(C)]
pub struct GopMode {
    pub max_mode: u32,
    pub mode: u32,
    pub info: *mut GopModeInfo,
    pub size_of_info: usize,
    pub frame_buffer_base: u64,
    pub frame_buffer_size: usize,
}

#[repr(C)]
pub struct GopModeInfo {
    pub version: u32,
    pub horizontal_resolution: u32,
    pub vertical_resolution: u32,
    pub pixel_format: u32,
    pub pixel_information: [u32; 4],
    pub pixels_per_scan_line: u32,
}
