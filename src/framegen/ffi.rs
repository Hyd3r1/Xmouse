//! Minimal declarations for the two vendor runtimes the frame-generation route loads at run
//! time, and the shaders it compiles. Nothing from the vendors is linked or shipped.
//!
//! `ffx` and `sl` are our own transcriptions of struct layouts and constants from two
//! MIT-licensed public header sets:
//!   AMD FidelityFX API        Copyright (C) Advanced Micro Devices, Inc.
//!   NVIDIA Streamline         Copyright (c) NVIDIA CORPORATION
//! Permission is hereby granted, free of charge, to any person obtaining a copy of this
//! software and associated documentation files (the "Software"), to deal in the Software
//! without restriction, including without limitation the rights to use, copy, modify, merge,
//! publish, distribute, sublicense, and/or sell copies of the Software, and to permit
//! persons to whom the Software is furnished to do so, subject to the following conditions:
//! The above copyright notice and this permission notice shall be included in all copies or
//! substantial portions of the Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY
//! OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//! MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
//! THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
//! WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
//! CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
#![allow(dead_code)]
pub mod ffx {
    use std::ffi::c_void;

    pub const CREATE_FG: u64 = 0x0002_0001;
    pub const CONFIGURE_FG: u64 = 0x0002_0002;
    pub const PREPARE_V1: u64 = 0x0002_0004;
    pub const PREPARE_V2: u64 = 0x0002_000c;
    pub const CREATE_FG_VERSION: u64 = 0x0002_000e;
    pub const FG_VERSION: u32 = (4 << 22) | 2;
    pub const CREATE_SWAPCHAIN_FOR_HWND: u64 = 0x0003_0006;
    pub const SWAPCHAIN_WAIT_FOR_PRESENTS: u64 = 0x0003_0007;
    pub const CREATE_SWAPCHAIN_VERSION: u64 = 0x0003_000b;
    pub const SWAPCHAIN_VERSION: u32 = 4 << 22;
    pub const CREATE_BACKEND_DX12: u64 = 0x0000_0002;
    pub const QUERY_GET_VERSIONS: u64 = 4;
    pub const CONFIGURE_GLOBAL_DEBUG: u64 = 7;

    pub const FORMAT_R8G8B8A8_UNORM: u32 = 10;
    pub const FORMAT_R16G16_FLOAT: u32 = 18;
    pub const FORMAT_R32_FLOAT: u32 = 28;
    pub const RESOURCE_TYPE_TEXTURE2D: u32 = 2;
    pub const USAGE_UAV: u32 = 2;
    pub const STATE_COMPUTE_READ: u32 = 4;

    #[repr(C)]
    pub struct Header {
        pub ty: u64,
        pub next: *mut Header,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct Dim2 {
        pub width: u32,
        pub height: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct Float2 {
        pub x: f32,
        pub y: f32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub width: i32,
        pub height: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct ResourceDesc {
        pub ty: u32,
        pub format: u32,
        pub width: u32,
        pub height: u32,
        pub depth: u32,
        pub mip_count: u32,
        pub flags: u32,
        pub usage: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Resource {
        pub resource: *mut c_void,
        pub desc: ResourceDesc,
        pub state: u32,
    }
    impl Resource {
        pub fn empty() -> Resource {
            Resource { resource: std::ptr::null_mut(), desc: ResourceDesc::default(), state: 0 }
        }
    }

    #[repr(C)]
    pub struct CreateSwapChainForHwnd {
        pub header: Header,
        pub swapchain: *mut *mut c_void,
        pub hwnd: *mut c_void,
        pub desc: *const c_void,
        pub fullscreen_desc: *const c_void,
        pub dxgi_factory: *mut c_void,
        pub game_queue: *mut c_void,
    }
    #[repr(C)]
    pub struct CreateVersion {
        pub header: Header,
        pub version: u32,
    }
    #[repr(C)]
    pub struct CreateBackendDx12 {
        pub header: Header,
        pub device: *mut c_void,
    }
    #[repr(C)]
    pub struct CreateFg {
        pub header: Header,
        pub flags: u32,
        pub display_size: Dim2,
        pub max_render_size: Dim2,
        pub back_buffer_format: u32,
    }

    /// What the swapchain hands to the present callback for every frame it shows.
    #[repr(C)]
    pub struct PresentCallbackDesc {
        pub header: Header,
        pub device: *mut c_void,
        pub command_list: *mut c_void,
        pub current_back_buffer: Resource,
        pub current_ui: Resource,
        pub output_swap_chain_buffer: Resource,
        pub is_generated_frame: bool,
        pub frame_id: u64,
    }
    pub type PresentCallback = unsafe extern "C" fn(*mut PresentCallbackDesc, *mut c_void) -> u32;
    pub type GenerateCallback = unsafe extern "C" fn(*mut Header, *mut c_void) -> u32;

    #[repr(C)]
    pub struct ConfigureFg {
        pub header: Header,
        pub swap_chain: *mut c_void,
        pub present_callback: Option<PresentCallback>,
        pub present_callback_user_context: *mut c_void,
        pub frame_generation_callback: Option<GenerateCallback>,
        pub frame_generation_callback_user_context: *mut c_void,
        pub frame_generation_enabled: bool,
        pub allow_async_workloads: bool,
        pub hudless_color: Resource,
        pub flags: u32,
        pub only_present_generated: bool,
        pub generation_rect: Rect,
        pub frame_id: u64,
    }

    /// The older prepare descriptor is a strict prefix of this one, so a runtime that
    /// does not know V2 can be given the same bytes under the older type id.
    #[repr(C)]
    pub struct PrepareV2 {
        pub header: Header,
        pub frame_id: u64,
        pub flags: u32,
        pub command_list: *mut c_void,
        pub render_size: Dim2,
        pub jitter_offset: Float2,
        pub motion_vector_scale: Float2,
        pub frame_time_delta: f32,
        pub reset: bool,
        pub camera_near: f32,
        pub camera_far: f32,
        pub camera_fov_angle_vertical: f32,
        pub view_space_to_meters_factor: f32,
        pub depth: Resource,
        pub motion_vectors: Resource,
        pub camera_position: [f32; 3],
        pub camera_up: [f32; 3],
        pub camera_right: [f32; 3],
        pub camera_forward: [f32; 3],
    }

    #[repr(C)]
    pub struct QueryGetVersions {
        pub header: Header,
        pub create_desc_type: u64,
        pub device: *mut c_void,
        pub output_count: *mut u64,
        pub version_ids: *mut u64,
        pub version_names: *mut *const std::ffi::c_char,
    }
    #[repr(C)]
    pub struct ConfigureGlobalDebug {
        pub header: Header,
        pub effect_id: u64,
        pub message: Option<unsafe extern "C" fn(u32, *const u16)>,
        pub debug_level: u32,
    }

    pub type CreateContext = unsafe extern "C" fn(*mut *mut c_void, *mut Header, *const c_void) -> u32;
    pub type DestroyContext = unsafe extern "C" fn(*mut *mut c_void, *const c_void) -> u32;
    pub type Configure = unsafe extern "C" fn(*mut *mut c_void, *const Header) -> u32;
    pub type Query = unsafe extern "C" fn(*mut *mut c_void, *mut Header) -> u32;
    pub type Dispatch = unsafe extern "C" fn(*mut *mut c_void, *const Header) -> u32;

    const _: () = assert!(std::mem::size_of::<Resource>() == 48);
    const _: () = assert!(std::mem::size_of::<PresentCallbackDesc>() == 192);

    pub fn code_name(c: u32) -> &'static str {
        match c {
            0 => "OK",
            1 => "ERROR",
            2 => "ERROR_UNKNOWN_DESCTYPE",
            3 => "ERROR_RUNTIME_ERROR",
            4 => "NO_PROVIDER",
            5 => "ERROR_MEMORY",
            6 => "ERROR_PARAMETER",
            7 => "PROVIDER_NO_SUPPORT_NEW_DESCTYPE",
            _ => "?",
        }
    }
}

pub mod sl {
    use std::ffi::{c_char, c_void};

    pub const SDK_VERSION: u64 = (2u64 << 48) | (14u64 << 32) | (16u64 << 16) | 0xfedc;
    pub const FEATURE_REFLEX: u32 = 3;
    pub const FEATURE_PCL: u32 = 4;
    pub const FEATURE_DLSS_G: u32 = 1000;
    pub const BUFFER_DEPTH: u32 = 0;
    pub const BUFFER_MOTION_VECTORS: u32 = 1;
    pub const LIFECYCLE_VALID_UNTIL_PRESENT: u32 = 1;
    pub const PREF_DISABLE_CL_STATE_TRACKING: u64 = 1 << 0;
    pub const PREF_FRAME_BASED_TAGGING: u64 = 1 << 7;
    pub const REQ_HARDWARE_SCHEDULING: u32 = 1 << 4;
    pub const DLSSG_MODE_OFF: u32 = 0;
    pub const DLSSG_MODE_ON: u32 = 1;
    pub const DLSSG_FLAG_SHOW_ONLY_INTERPOLATED: u32 = 1 << 0;
    pub const REFLEX_LOW_LATENCY: u32 = 1;
    pub const INVALID_FLOAT: f32 = f32::MAX;
    pub const FALSE: u8 = 0;
    pub const TRUE: u8 = 1;
    pub const INVALID: u8 = 2;

    pub const PCL_SIMULATION_START: u32 = 0;
    pub const PCL_SIMULATION_END: u32 = 1;
    pub const PCL_RENDER_SUBMIT_START: u32 = 2;
    pub const PCL_RENDER_SUBMIT_END: u32 = 3;
    pub const PCL_PRESENT_START: u32 = 4;
    pub const PCL_PRESENT_END: u32 = 5;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

    #[repr(C)]
    pub struct Base {
        pub next: *mut c_void,
        pub ty: Guid,
        pub version: usize,
    }
    impl Base {
        pub const fn new(ty: Guid, version: usize) -> Base {
            Base { next: std::ptr::null_mut(), ty, version }
        }
    }

    const G_PREFERENCES: Guid = Guid(0x1ca10965, 0xbf8e, 0x432b, [0x8d, 0xa1, 0x67, 0x16, 0xd8, 0x79, 0xfb, 0x14]);
    const G_RESOURCE: Guid = Guid(0x3a9d70cf, 0x2418, 0x4b72, [0x83, 0x91, 0x13, 0xf8, 0x72, 0x1c, 0x72, 0x61]);
    const G_RESOURCE_TAG: Guid = Guid(0x4c6a5aad, 0xb445, 0x496c, [0x87, 0xff, 0x1a, 0xf3, 0x84, 0x5b, 0xe6, 0x53]);
    const G_VIEWPORT: Guid = Guid(0x171b6435, 0x9b3c, 0x4fc8, [0x99, 0x94, 0xfb, 0xe5, 0x25, 0x69, 0xaa, 0xa4]);
    const G_REQUIREMENTS: Guid = Guid(0x66714097, 0xac6d, 0x4bc6, [0x89, 0x15, 0x1e, 0x0f, 0x55, 0xa6, 0xb6, 0x1f]);
    const G_ADAPTER_INFO: Guid = Guid(0x0677315f, 0xa746, 0x4492, [0x9f, 0x42, 0xcb, 0x61, 0x42, 0xc9, 0xc3, 0xd4]);
    const G_CONSTANTS: Guid = Guid(0xdcd35ad7, 0x4e4a, 0x4bad, [0xa9, 0x0c, 0xe0, 0xc4, 0x9e, 0xb2, 0x3a, 0xfe]);
    const G_DLSSG_OPTIONS: Guid = Guid(0xfac5f1cb, 0x2dfd, 0x4f36, [0xa1, 0xe6, 0x3a, 0x9e, 0x86, 0x52, 0x56, 0xc5]);
    const G_DLSSG_STATE: Guid = Guid(0xcc8ac8e1, 0xa179, 0x44f5, [0x97, 0xfa, 0xe7, 0x41, 0x12, 0xf9, 0xbc, 0x61]);
    const G_REFLEX_OPTIONS: Guid = Guid(0xf03af81a, 0x6d0b, 0x4902, [0xa6, 0x51, 0xc4, 0x96, 0x5e, 0x21, 0x54, 0x34]);

    pub type LogCallback = unsafe extern "C" fn(u32, *const c_char);

    #[repr(C)]
    pub struct Preferences {
        pub base: Base,
        pub show_console: bool,
        pub log_level: u32,
        pub paths_to_plugins: *const *const u16,
        pub num_paths_to_plugins: u32,
        pub path_to_logs_and_data: *const u16,
        pub allocate_callback: *const c_void,
        pub release_callback: *const c_void,
        pub log_message_callback: Option<LogCallback>,
        pub flags: u64,
        pub features_to_load: *const u32,
        pub num_features_to_load: u32,
        pub application_id: u32,
        pub engine: u32,
        pub engine_version: *const c_char,
        pub project_id: *const c_char,
        pub render_api: u32,
    }
    impl Preferences {
        pub fn new() -> Preferences {
            let mut p: Preferences = unsafe { std::mem::zeroed() };
            p.base = Base::new(G_PREFERENCES, 1);
            p.log_level = 1;
            p.render_api = 1; // D3D12
            p
        }
    }

    #[repr(C)]
    pub struct Resource {
        pub base: Base,
        pub ty: u8,
        pub native: *mut c_void,
        pub memory: *mut c_void,
        pub view: *mut c_void,
        pub state: u32,
        pub width: u32,
        pub height: u32,
        pub native_format: u32,
        pub mip_levels: u32,
        pub array_layers: u32,
        pub gpu_virtual_address: u64,
        pub flags: u32,
        pub usage: u32,
        pub internal_flags: u16,
        pub reserved: u16,
    }
    impl Resource {
        pub fn tex2d(native: *mut c_void, state: u32, width: u32, height: u32, format: u32) -> Resource {
            let mut r: Resource = unsafe { std::mem::zeroed() };
            r.base = Base::new(G_RESOURCE, 1);
            r.native = native;
            r.state = state;
            r.width = width;
            r.height = height;
            r.native_format = format;
            r.mip_levels = 1;
            r.array_layers = 1;
            r
        }
    }

    #[repr(C)]
    pub struct ResourceTag {
        pub base: Base,
        pub resource: *mut Resource,
        pub ty: u32,
        pub lifecycle: u32,
        pub extent: [u32; 4],
    }
    impl ResourceTag {
        pub fn new(resource: *mut Resource, ty: u32, lifecycle: u32) -> ResourceTag {
            ResourceTag { base: Base::new(G_RESOURCE_TAG, 1), resource, ty, lifecycle, extent: [0; 4] }
        }
    }

    #[repr(C)]
    pub struct ViewportHandle {
        pub base: Base,
        pub value: u32,
    }
    impl ViewportHandle {
        pub fn new(value: u32) -> ViewportHandle {
            ViewportHandle { base: Base::new(G_VIEWPORT, 1), value }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default, Debug)]
    pub struct Version {
        pub major: u32,
        pub minor: u32,
        pub build: u32,
    }

    #[repr(C)]
    pub struct FeatureRequirements {
        pub base: Base,
        pub flags: u32,
        pub max_num_cpu_threads: u32,
        pub max_num_viewports: u32,
        pub num_required_tags: u32,
        pub required_tags: *const u32,
        pub os_version_detected: Version,
        pub os_version_required: Version,
        pub driver_version_detected: Version,
        pub driver_version_required: Version,
        pub vk_num_compute_queues_required: u32,
        pub vk_num_graphics_queues_required: u32,
        pub vk_num_device_extensions: u32,
        pub vk_device_extensions: *const *const c_char,
        pub vk_num_instance_extensions: u32,
        pub vk_instance_extensions: *const *const c_char,
        pub vk_num_features12: u32,
        pub vk_features12: *const *const c_char,
        pub vk_num_features13: u32,
        pub vk_features13: *const *const c_char,
        pub vk_num_optical_flow_queues_required: u32,
    }
    impl FeatureRequirements {
        pub fn new() -> FeatureRequirements {
            let mut r: FeatureRequirements = unsafe { std::mem::zeroed() };
            r.base = Base::new(G_REQUIREMENTS, 2);
            r
        }
    }

    #[repr(C)]
    pub struct AdapterInfo {
        pub base: Base,
        pub device_luid: *const u8,
        pub device_luid_size_in_bytes: u32,
        pub vk_physical_device: *mut c_void,
    }
    impl AdapterInfo {
        pub fn new(luid: *const u8, size: u32) -> AdapterInfo {
            AdapterInfo {
                base: Base::new(G_ADAPTER_INFO, 1),
                device_luid: luid,
                device_luid_size_in_bytes: size,
                vk_physical_device: std::ptr::null_mut(),
            }
        }
    }

    #[repr(C)]
    pub struct Constants {
        pub base: Base,
        pub camera_view_to_clip: [[f32; 4]; 4],
        pub clip_to_camera_view: [[f32; 4]; 4],
        pub clip_to_lens_clip: [[f32; 4]; 4],
        pub clip_to_prev_clip: [[f32; 4]; 4],
        pub prev_clip_to_clip: [[f32; 4]; 4],
        pub jitter_offset: [f32; 2],
        pub mvec_scale: [f32; 2],
        pub camera_pinhole_offset: [f32; 2],
        pub camera_pos: [f32; 3],
        pub camera_up: [f32; 3],
        pub camera_right: [f32; 3],
        pub camera_fwd: [f32; 3],
        pub camera_near: f32,
        pub camera_far: f32,
        pub camera_fov: f32,
        pub camera_aspect_ratio: f32,
        pub motion_vectors_invalid_value: f32,
        pub depth_inverted: u8,
        pub camera_motion_included: u8,
        pub motion_vectors_3d: u8,
        pub reset: u8,
        pub orthographic_projection: u8,
        pub motion_vectors_dilated: u8,
        pub motion_vectors_jittered: u8,
        pub min_relative_linear_depth_object_separation: f32,
    }
    impl Constants {
        pub fn new() -> Constants {
            let mut c: Constants = unsafe { std::mem::zeroed() };
            c.base = Base::new(G_CONSTANTS, 2);
            c.min_relative_linear_depth_object_separation = 40.0;
            c
        }
    }

    #[repr(C)]
    pub struct ApiError {
        pub hres: i32,
    }

    #[repr(C)]
    pub struct DlssgOptions {
        pub base: Base,
        pub mode: u32,
        pub num_frames_to_generate: u32,
        pub flags: u32,
        pub dynamic_res_width: u32,
        pub dynamic_res_height: u32,
        pub num_back_buffers: u32,
        pub mvec_depth_width: u32,
        pub mvec_depth_height: u32,
        pub color_width: u32,
        pub color_height: u32,
        pub color_buffer_format: u32,
        pub mvec_buffer_format: u32,
        pub depth_buffer_format: u32,
        pub hud_less_buffer_format: u32,
        pub ui_buffer_format: u32,
        pub on_error_callback: Option<unsafe extern "C" fn(*const ApiError)>,
        pub reserved15: u8,
        pub queue_parallelism_mode: u32,
        pub enable_user_interface_recomposition: u8,
        pub dynamic_target_frame_rate: f32,
    }
    impl DlssgOptions {
        pub fn new() -> DlssgOptions {
            let mut o: DlssgOptions = unsafe { std::mem::zeroed() };
            o.base = Base::new(G_DLSSG_OPTIONS, 5);
            o.num_frames_to_generate = 1;
            o.reserved15 = INVALID;
            o
        }
    }

    #[repr(C)]
    pub struct DlssgState {
        pub base: Base,
        pub estimated_vram_usage_in_bytes: u64,
        pub status: u32,
        pub min_width_or_height: u32,
        pub num_frames_actually_presented: u32,
        pub num_frames_to_generate_max: u32,
        pub reserved4: u8,
        pub is_vsync_support_available: u8,
        pub inputs_processing_completion_fence: *mut c_void,
        pub last_present_inputs_processing_completion_fence_value: u64,
        pub is_dynamic_mfg_supported: u8,
    }
    impl DlssgState {
        pub fn new() -> DlssgState {
            let mut s: DlssgState = unsafe { std::mem::zeroed() };
            s.base = Base::new(G_DLSSG_STATE, 4);
            s
        }
    }

    #[repr(C)]
    pub struct ReflexOptions {
        pub base: Base,
        pub mode: u32,
        pub frame_limit_us: u32,
        pub use_markers_to_optimize: bool,
        pub virtual_key: u16,
        pub id_thread: u32,
    }
    impl ReflexOptions {
        pub fn new(mode: u32) -> ReflexOptions {
            let mut o: ReflexOptions = unsafe { std::mem::zeroed() };
            o.base = Base::new(G_REFLEX_OPTIONS, 1);
            o.mode = mode;
            o
        }
    }

    const _: () = assert!(std::mem::size_of::<Base>() == 32);
    const _: () = assert!(std::mem::size_of::<Preferences>() == 144);
    const _: () = assert!(std::mem::size_of::<Resource>() == 112);
    const _: () = assert!(std::mem::size_of::<ResourceTag>() == 64);
    const _: () = assert!(std::mem::size_of::<ViewportHandle>() == 40);
    const _: () = assert!(std::mem::size_of::<FeatureRequirements>() == 184);
    const _: () = assert!(std::mem::size_of::<Constants>() == 456);
    const _: () = assert!(std::mem::size_of::<DlssgOptions>() == 120);
    const _: () = assert!(std::mem::size_of::<DlssgState>() == 88);
    const _: () = assert!(std::mem::size_of::<ReflexOptions>() == 48);

    // References in the C++ signatures are pointers at the ABI level; a frame token is
    // only ever passed back, so it stays an opaque pointer.
    pub type Init = unsafe extern "C" fn(*const Preferences, u64) -> i32;
    pub type Shutdown = unsafe extern "C" fn() -> i32;
    pub type IsFeatureSupported = unsafe extern "C" fn(u32, *const AdapterInfo) -> i32;
    pub type GetFeatureRequirements = unsafe extern "C" fn(u32, *mut FeatureRequirements) -> i32;
    pub type SetD3DDevice = unsafe extern "C" fn(*mut c_void) -> i32;
    pub type GetNativeInterface = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32;
    pub type GetFeatureFunction = unsafe extern "C" fn(u32, *const c_char, *mut *mut c_void) -> i32;
    pub type GetNewFrameToken = unsafe extern "C" fn(*mut *mut c_void, *const u32) -> i32;
    pub type SetConstants = unsafe extern "C" fn(*const Constants, *const c_void, *const ViewportHandle) -> i32;
    pub type SetTagForFrame =
        unsafe extern "C" fn(*const c_void, *const ViewportHandle, *const ResourceTag, u32, *mut c_void) -> i32;
    pub type DlssgGetState =
        unsafe extern "C" fn(*const ViewportHandle, *mut DlssgState, *const DlssgOptions) -> i32;
    pub type DlssgSetOptions = unsafe extern "C" fn(*const ViewportHandle, *const DlssgOptions) -> i32;
    pub type ReflexSetOptions = unsafe extern "C" fn(*const ReflexOptions) -> i32;
    pub type ReflexSleep = unsafe extern "C" fn(*const c_void) -> i32;
    pub type PclSetMarker = unsafe extern "C" fn(u32, *const c_void) -> i32;

    pub fn result_name(r: i32) -> &'static str {
        const NAMES: [&str; 40] = [
            "eOk",
            "eErrorIO",
            "eErrorDriverOutOfDate",
            "eErrorOSOutOfDate",
            "eErrorOSDisabledHWS",
            "eErrorDeviceNotCreated",
            "eErrorNoSupportedAdapterFound",
            "eErrorAdapterNotSupported",
            "eErrorNoPlugins",
            "eErrorVulkanAPI",
            "eErrorDXGIAPI",
            "eErrorD3DAPI",
            "eErrorNRDAPI",
            "eErrorNVAPI",
            "eErrorReflexAPI",
            "eErrorNGXFailed",
            "eErrorJSONParsing",
            "eErrorMissingProxy",
            "eErrorMissingResourceState",
            "eErrorInvalidIntegration",
            "eErrorMissingInputParameter",
            "eErrorNotInitialized",
            "eErrorComputeFailed",
            "eErrorInitNotCalled",
            "eErrorExceptionHandler",
            "eErrorInvalidParameter",
            "eErrorMissingConstants",
            "eErrorDuplicatedConstants",
            "eErrorMissingOrInvalidAPI",
            "eErrorCommonConstantsMissing",
            "eErrorUnsupportedInterface",
            "eErrorFeatureMissing",
            "eErrorFeatureNotSupported",
            "eErrorFeatureMissingHooks",
            "eErrorFeatureFailedToLoad",
            "eErrorFeatureWrongPriority",
            "eErrorFeatureMissingDependency",
            "eErrorFeatureManagerInvalidState",
            "eErrorInvalidState",
            "eWarnOutOfVRAM",
        ];
        NAMES.get(r as usize).copied().unwrap_or("?")
    }

    pub fn status_names(s: u32) -> String {
        if s == 0 {
            return "eOk".into();
        }
        let bits = [
            (1, "eFailResolutionTooLow"),
            (2, "eFailReflexNotDetectedAtRuntime"),
            (4, "eFailHDRFormatNotSupported"),
            (8, "eFailCommonConstantsInvalid"),
            (16, "eFailGetCurrentBackBufferIndexNotCalled"),
            (32, "eReserved5"),
        ];
        bits.iter().filter(|(b, _)| s & b != 0).map(|(_, n)| *n).collect::<Vec<_>>().join("|")
    }
}

pub const HLSL: &str = r#"
cbuffer C : register(b0) {
    int2 dispSize; int2 gridSize;
    int2 qSize; int coarseRange; int pad0;
    float2 panV; float2 objV;
    float2 objC; float objR; float pad1;
    float4 hudRect;
    float4 hud2Rect;
};
Texture2D<float4> t0 : register(t0);
Texture2D<float4> t1 : register(t1);
Texture2D<float4> t2 : register(t2);
Texture2D<float4> t3 : register(t3);
RWTexture2D<float4> u0 : register(u0);
SamplerState sLin : register(s0);

float luma(float3 c) { return dot(c, float3(0.299, 0.587, 0.114)); }

struct VO { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VO vsMain(uint id : SV_VertexID) {
    VO o;
    o.uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(o.uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}
// The "video frame" scaled to the window, as the app's resample pass would.
float4 psBlit(VO i) : SV_Target { return float4(t0.SampleLevel(sLin, i.uv, 0).rgb, 1); }

// Quarter-resolution luma for the coarse search.
[numthreads(8, 8, 1)]
void csLuma(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)qSize.x || id.y >= (uint)qSize.y) return;
    float s = 0;
    for (int y = 0; y < 4; y++)
        for (int x = 0; x < 4; x++)
            s += luma(t0.Load(int3(min(int2(id.xy) * 4 + int2(x, y), dispSize - 1), 0)).rgb);
    u0[id.xy] = float4(s / 16.0, 0, 0, 0);
}

// One vector per 8x8 block: exhaustive search at quarter resolution over a 24x24 pixel
// window. A small cost on length keeps flat areas at rest.
[numthreads(8, 8, 1)]
void csCoarse(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)gridSize.x || id.y >= (uint)gridSize.y) return;
    int2 cq = int2(id.xy) * 2 + 1;
    float cur[36];
    int i, j;
    for (j = 0; j < 6; j++)
        for (i = 0; i < 6; i++)
            cur[j * 6 + i] = t0.Load(int3(clamp(cq + int2(i - 3, j - 3), 0, qSize - 1), 0)).r;
    float best = 1e9;
    int2 bd = int2(0, 0);
    [loop] for (int dy = -coarseRange; dy <= coarseRange; dy++) {
        [loop] for (int dx = -coarseRange; dx <= coarseRange; dx++) {
            float sad = 0.004 * (abs(dx) + abs(dy));
            for (j = 0; j < 6; j++)
                for (i = 0; i < 6; i++)
                    sad += abs(cur[j * 6 + i] - t1.Load(int3(clamp(cq + int2(i - 3 + dx, j - 3 + dy), 0, qSize - 1), 0)).r);
            if (sad < best) { best = sad; bd = int2(dx, dy); }
        }
    }
    u0[id.xy] = float4(bd * 4.0, 0, 0);
}

// Refines each block vector to whole pixels at full resolution.
[numthreads(8, 8, 1)]
void csRefine(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)gridSize.x || id.y >= (uint)gridSize.y) return;
    int2 c = int2(id.xy) * 8 + 4;
    int2 base = int2(round(t2.Load(int3(id.xy, 0)).xy));
    float cur[36];
    int i, j;
    for (j = 0; j < 6; j++)
        for (i = 0; i < 6; i++)
            cur[j * 6 + i] = luma(t0.Load(int3(clamp(c + int2(i - 3, j - 3) * 2, 0, dispSize - 1), 0)).rgb);
    float best = 1e9;
    int2 bd = base;
    [loop] for (int dy = -3; dy <= 3; dy++) {
        [loop] for (int dx = -3; dx <= 3; dx++) {
            int2 d = base + int2(dx, dy);
            float sad = 0.002 * (abs(d.x) + abs(d.y));
            for (j = 0; j < 6; j++)
                for (i = 0; i < 6; i++)
                    sad += abs(cur[j * 6 + i] - luma(t1.Load(int3(clamp(c + int2(i - 3, j - 3) * 2 + d, 0, dispSize - 1), 0)).rgb));
            if (sad < best) { best = sad; bd = d; }
        }
    }
    u0[id.xy] = float4(bd, 0, 0);
}

float med9(float v[9]) {
    for (int a = 0; a < 5; a++)
        for (int b = a + 1; b < 9; b++)
            if (v[b] < v[a]) { float t = v[a]; v[a] = v[b]; v[b] = t; }
    return v[4];
}

// Per-component 3x3 median: removes lone wrong blocks.
[numthreads(8, 8, 1)]
void csMedian(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)gridSize.x || id.y >= (uint)gridSize.y) return;
    float xs[9]; float ys[9];
    int k = 0;
    for (int j = -1; j <= 1; j++)
        for (int i = -1; i <= 1; i++) {
            float2 v = t0.Load(int3(clamp(int2(id.xy) + int2(i, j), 0, gridSize - 1), 0)).xy;
            xs[k] = v.x; ys[k] = v.y; k++;
        }
    u0[id.xy] = float4(med9(xs), med9(ys), 0, 0);
}

// Dense field: every pixel takes whichever of the four surrounding block vectors explains
// it best, so motion edges follow the picture instead of the block grid.
[numthreads(8, 8, 1)]
void csUpsample(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)dispSize.x || id.y >= (uint)dispSize.y) return;
    int2 p = int2(id.xy);
    float2 bf = (float2(p) + 0.5) / 8.0 - 0.5;
    int2 b0 = int2(floor(bf));
    float best = 1e9;
    float2 bv = float2(0, 0);
    for (int k = 0; k < 4; k++) {
        int2 b = clamp(b0 + int2(k & 1, k >> 1), 0, gridSize - 1);
        float2 v = t2.Load(int3(b, 0)).xy;
        int2 d = int2(round(v));
        float2 dist = abs(bf - float2(b));
        float e = 0.01 * (dist.x + dist.y);
        for (int j = -1; j <= 1; j++)
            for (int i = -1; i <= 1; i++) {
                int2 q = clamp(p + int2(i, j) * 2, 0, dispSize - 1);
                e += abs(luma(t0.Load(int3(q, 0)).rgb) - luma(t1.Load(int3(clamp(q + d, 0, dispSize - 1), 0)).rgb));
            }
        if (e < best) { best = e; bv = v; }
    }
    u0[p] = float4(bv, 0, 0);
}

// Comparison mode only: the vectors the clip was built from.
[numthreads(8, 8, 1)]
void csTruth(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)dispSize.x || id.y >= (uint)dispSize.y) return;
    float2 p = float2(id.xy) + 0.5;
    float2 v = panV;
    if (length(p - objC) <= objR) v = objV;
    if (all(p >= hudRect.xy) && all(p < hudRect.zw)) v = float2(0, 0);
    if (all(p >= hud2Rect.xy) && all(p < hud2Rect.zw)) v = float2(0, 0);
    u0[id.xy] = float4(v, 0, 0);
}
"#;
