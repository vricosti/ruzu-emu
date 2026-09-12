use common::ResultCode;

pub const MODULE_AUDIO: u32 = 153;
pub const MODULE_HWOPUS: u32 = 111;

pub const RESULT_NOT_FOUND: ResultCode = ResultCode::new(MODULE_AUDIO, 1);
pub const RESULT_OPERATION_FAILED: ResultCode = ResultCode::new(MODULE_AUDIO, 2);
pub const RESULT_INVALID_SAMPLE_RATE: ResultCode = ResultCode::new(MODULE_AUDIO, 3);
pub const RESULT_INSUFFICIENT_BUFFER: ResultCode = ResultCode::new(MODULE_AUDIO, 4);
pub const RESULT_OUT_OF_SESSIONS: ResultCode = ResultCode::new(MODULE_AUDIO, 5);
pub const RESULT_BUFFER_COUNT_REACHED: ResultCode = ResultCode::new(MODULE_AUDIO, 8);
pub const RESULT_INVALID_CHANNEL_COUNT: ResultCode = ResultCode::new(MODULE_AUDIO, 10);
pub const RESULT_INVALID_UPDATE_INFO: ResultCode = ResultCode::new(MODULE_AUDIO, 41);
pub const RESULT_INVALID_ADDRESS_INFO: ResultCode = ResultCode::new(MODULE_AUDIO, 42);
pub const RESULT_NOT_SUPPORTED: ResultCode = ResultCode::new(MODULE_AUDIO, 513);
pub const RESULT_INVALID_HANDLE: ResultCode = ResultCode::new(MODULE_AUDIO, 1536);
pub const RESULT_INVALID_REVISION: ResultCode = ResultCode::new(MODULE_AUDIO, 1537);

pub const RESULT_LIB_OPUS_ALLOC_FAIL: ResultCode = ResultCode::new(MODULE_HWOPUS, 7);
pub const RESULT_INPUT_DATA_TOO_SMALL: ResultCode = ResultCode::new(MODULE_HWOPUS, 8);
pub const RESULT_LIB_OPUS_INVALID_STATE: ResultCode = ResultCode::new(MODULE_HWOPUS, 6);
pub const RESULT_LIB_OPUS_UNIMPLEMENTED: ResultCode = ResultCode::new(MODULE_HWOPUS, 5);
pub const RESULT_LIB_OPUS_INVALID_PACKET: ResultCode = ResultCode::new(MODULE_HWOPUS, 17);
pub const RESULT_LIB_OPUS_INTERNAL_ERROR: ResultCode = ResultCode::new(MODULE_HWOPUS, 4);
pub const RESULT_BUFFER_TOO_SMALL: ResultCode = ResultCode::new(MODULE_HWOPUS, 3);
pub const RESULT_LIB_OPUS_BAD_ARG: ResultCode = ResultCode::new(MODULE_HWOPUS, 2);
pub const RESULT_INVALID_OPUS_DSP_RETURN_CODE: ResultCode = ResultCode::new(MODULE_HWOPUS, 259);
pub const RESULT_OUT_OF_OPUS_DECODERS: ResultCode = ResultCode::new(MODULE_HWOPUS, 385);
pub const RESULT_INVALID_OPUS_SAMPLE_RATE: ResultCode = ResultCode::new(MODULE_HWOPUS, 1001);
pub const RESULT_INVALID_OPUS_CHANNEL_COUNT: ResultCode = ResultCode::new(MODULE_HWOPUS, 1002);
