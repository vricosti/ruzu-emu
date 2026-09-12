//! Port of zuyu/src/core/hle/service/audio/errors.h
//!
//! Audio service error codes.

/// Error module for Audio services.
/// Port of ErrorModule::Audio
pub const ERROR_MODULE_AUDIO: u32 = 153; // placeholder module id

/// Error module for HwOpus services.
/// Port of ErrorModule::HwOpus
pub const ERROR_MODULE_HW_OPUS: u32 = 154; // placeholder module id

// Audio error codes -- correspond to constexpr Result in errors.h
pub const RESULT_NOT_FOUND: (u32, u32) = (ERROR_MODULE_AUDIO, 1);
pub const RESULT_OPERATION_FAILED: (u32, u32) = (ERROR_MODULE_AUDIO, 2);
pub const RESULT_INVALID_SAMPLE_RATE: (u32, u32) = (ERROR_MODULE_AUDIO, 3);
pub const RESULT_INSUFFICIENT_BUFFER: (u32, u32) = (ERROR_MODULE_AUDIO, 4);
pub const RESULT_OUT_OF_SESSIONS: (u32, u32) = (ERROR_MODULE_AUDIO, 5);
pub const RESULT_BUFFER_COUNT_REACHED: (u32, u32) = (ERROR_MODULE_AUDIO, 8);
pub const RESULT_INVALID_CHANNEL_COUNT: (u32, u32) = (ERROR_MODULE_AUDIO, 10);
pub const RESULT_INVALID_UPDATE_INFO: (u32, u32) = (ERROR_MODULE_AUDIO, 41);
pub const RESULT_INVALID_ADDRESS_INFO: (u32, u32) = (ERROR_MODULE_AUDIO, 42);
pub const RESULT_NOT_SUPPORTED: (u32, u32) = (ERROR_MODULE_AUDIO, 513);
pub const RESULT_INVALID_HANDLE: (u32, u32) = (ERROR_MODULE_AUDIO, 1536);
pub const RESULT_INVALID_REVISION: (u32, u32) = (ERROR_MODULE_AUDIO, 1537);

// HwOpus error codes
pub const RESULT_LIB_OPUS_ALLOC_FAIL: (u32, u32) = (ERROR_MODULE_HW_OPUS, 7);
pub const RESULT_INPUT_DATA_TOO_SMALL: (u32, u32) = (ERROR_MODULE_HW_OPUS, 8);
pub const RESULT_LIB_OPUS_INVALID_STATE: (u32, u32) = (ERROR_MODULE_HW_OPUS, 6);
pub const RESULT_LIB_OPUS_UNIMPLEMENTED: (u32, u32) = (ERROR_MODULE_HW_OPUS, 5);
pub const RESULT_LIB_OPUS_INVALID_PACKET: (u32, u32) = (ERROR_MODULE_HW_OPUS, 17);
pub const RESULT_LIB_OPUS_INTERNAL_ERROR: (u32, u32) = (ERROR_MODULE_HW_OPUS, 4);
pub const RESULT_BUFFER_TOO_SMALL: (u32, u32) = (ERROR_MODULE_HW_OPUS, 3);
pub const RESULT_LIB_OPUS_BAD_ARG: (u32, u32) = (ERROR_MODULE_HW_OPUS, 2);
pub const RESULT_INVALID_OPUS_DSP_RETURN_CODE: (u32, u32) = (ERROR_MODULE_HW_OPUS, 259);
pub const RESULT_OUT_OF_OPUS_DECODERS: (u32, u32) = (ERROR_MODULE_HW_OPUS, 385);
pub const RESULT_INVALID_OPUS_SAMPLE_RATE: (u32, u32) = (ERROR_MODULE_HW_OPUS, 1001);
pub const RESULT_INVALID_OPUS_CHANNEL_COUNT: (u32, u32) = (ERROR_MODULE_HW_OPUS, 1002);
