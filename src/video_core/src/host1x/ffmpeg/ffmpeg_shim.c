// SPDX-FileCopyrightText: Copyright 2026 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

#include <stdint.h>
#include <stdlib.h>
#include <errno.h>
#include <string.h>

#include <libavcodec/avcodec.h>
#include <libavcodec/codec.h>
#include <libavutil/avutil.h>
#include <libavutil/error.h>
#include <libavutil/frame.h>
#include <libavutil/hwcontext.h>
#include <libavutil/opt.h>
#include <libavutil/pixdesc.h>

#ifdef LIBVA_FOUND
#include <libavutil/hwcontext_vaapi.h>
#include <va/va.h>
#endif

typedef struct RuzuFfmpegDecoder {
    const AVCodec* decoder;
    AVCodecContext* context;
    int last_error;
} RuzuFfmpegDecoder;

typedef struct RuzuFfmpegHardwareContext {
    AVBufferRef* gpu_decoder;
    int last_error;
} RuzuFfmpegHardwareContext;

static const enum AVPixelFormat RUZU_PREFERRED_GPU_FORMAT = AV_PIX_FMT_NV12;
static const enum AVPixelFormat RUZU_PREFERRED_CPU_FORMAT = AV_PIX_FMT_YUV420P;

static const enum AVHWDeviceType RUZU_PREFERRED_GPU_DECODERS[] = {
#if defined(_WIN32)
    AV_HWDEVICE_TYPE_CUDA,
    AV_HWDEVICE_TYPE_D3D11VA,
    AV_HWDEVICE_TYPE_DXVA2,
    AV_HWDEVICE_TYPE_D3D12VA,
#elif defined(__FreeBSD__)
    AV_HWDEVICE_TYPE_VAAPI,
    AV_HWDEVICE_TYPE_VDPAU,
    AV_HWDEVICE_TYPE_DRM,
#elif defined(__APPLE__)
    AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
#elif defined(__ANDROID__)
    AV_HWDEVICE_TYPE_MEDIACODEC,
#elif defined(__unix__)
    AV_HWDEVICE_TYPE_CUDA,
    AV_HWDEVICE_TYPE_VAAPI,
    AV_HWDEVICE_TYPE_VDPAU,
#endif
    AV_HWDEVICE_TYPE_VULKAN,
};

#define RUZU_PREFERRED_GPU_DECODER_COUNT                                                 \
    (sizeof(RUZU_PREFERRED_GPU_DECODERS) / sizeof(RUZU_PREFERRED_GPU_DECODERS[0]))

static enum AVPixelFormat ruzu_get_gpu_format(AVCodecContext* codec_context,
                                              const enum AVPixelFormat* pix_fmts) {
    const AVPixFmtDescriptor* desc = av_pix_fmt_desc_get(codec_context->pix_fmt);
    if (desc != NULL && (desc->flags & AV_PIX_FMT_FLAG_HWACCEL) == 0) {
        for (int config_index = 0;; config_index++) {
            const AVCodecHWConfig* config =
                avcodec_get_hw_config(codec_context->codec, config_index);
            if (config == NULL) {
                break;
            }

            for (uintptr_t type_index = 0; type_index < RUZU_PREFERRED_GPU_DECODER_COUNT;
                 type_index++) {
                if ((config->methods & AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX) != 0 &&
                    config->device_type == RUZU_PREFERRED_GPU_DECODERS[type_index]) {
                    codec_context->pix_fmt = config->pix_fmt;
                }
            }
        }
    }

    for (const enum AVPixelFormat* p = pix_fmts; *p != AV_PIX_FMT_NONE; ++p) {
        if (*p == codec_context->pix_fmt) {
            return codec_context->pix_fmt;
        }
    }

    av_buffer_unref(&codec_context->hw_device_ctx);
    codec_context->pix_fmt = RUZU_PREFERRED_CPU_FORMAT;
    return codec_context->pix_fmt;
}

static enum AVCodecID ruzu_codec_id(uint64_t codec) {
    switch (codec) {
    case 0x3:
        return AV_CODEC_ID_H264;
    case 0x5:
        return AV_CODEC_ID_VP8;
    case 0x9:
        return AV_CODEC_ID_VP9;
    default:
        return AV_CODEC_ID_NONE;
    }
}

static const AVCodec* ruzu_find_decoder(uint64_t codec, int prefer_mediacodec) {
    const enum AVCodecID codec_id = ruzu_codec_id(codec);
    if (codec_id == AV_CODEC_ID_NONE) {
        return NULL;
    }

#if defined(__ANDROID__)
    if (prefer_mediacodec != 0) {
        const char* decoder_name = NULL;
        switch (codec_id) {
        case AV_CODEC_ID_H264:
            decoder_name = "h264_mediacodec";
            break;
        case AV_CODEC_ID_VP9:
            decoder_name = "vp9_mediacodec";
            break;
        default:
            break;
        }
        if (decoder_name != NULL) {
            const AVCodec* decoder = avcodec_find_decoder_by_name(decoder_name);
            if (decoder != NULL) {
                return decoder;
            }
        }
    }
#else
    (void)prefer_mediacodec;
#endif

    return avcodec_find_decoder(codec_id);
}

RuzuFfmpegDecoder* ruzu_ffmpeg_decoder_create(uint64_t codec, int prefer_mediacodec) {
    const AVCodec* decoder = ruzu_find_decoder(codec, prefer_mediacodec);
    if (decoder == NULL) {
        return NULL;
    }

    AVCodecContext* context = avcodec_alloc_context3(decoder);
    if (context == NULL) {
        return NULL;
    }

    av_opt_set(context->priv_data, "tune", "zerolatency", 0);
    context->thread_count = 0;
    context->thread_type &= ~FF_THREAD_FRAME;
#if defined(__ANDROID__)
    context->flags |= AV_CODEC_FLAG_LOW_DELAY;
    context->flags2 |= AV_CODEC_FLAG2_FAST;
#endif

    RuzuFfmpegDecoder* wrapper = calloc(1, sizeof(*wrapper));
    if (wrapper == NULL) {
        avcodec_free_context(&context);
        return NULL;
    }
    wrapper->decoder = decoder;
    wrapper->context = context;
    return wrapper;
}

const char* ruzu_ffmpeg_decoder_name(const RuzuFfmpegDecoder* decoder) {
    if (decoder == NULL || decoder->decoder == NULL) {
        return "";
    }
    return decoder->decoder->name;
}

void ruzu_ffmpeg_decoder_set_dimensions(RuzuFfmpegDecoder* decoder, int width, int height) {
    if (decoder == NULL || decoder->context == NULL) {
        return;
    }
    decoder->context->width = width;
    decoder->context->height = height;
    decoder->context->coded_width = width;
    decoder->context->coded_height = height;
}

int ruzu_ffmpeg_decoder_open(RuzuFfmpegDecoder* decoder, const uint8_t* extradata,
                             uintptr_t extradata_size) {
    if (decoder == NULL || decoder->context == NULL || decoder->decoder == NULL) {
        return -1;
    }

    if (extradata_size != 0) {
        av_freep(&decoder->context->extradata);
        decoder->context->extradata =
            av_mallocz(extradata_size + AV_INPUT_BUFFER_PADDING_SIZE);
        if (decoder->context->extradata == NULL) {
            decoder->last_error = AVERROR(ENOMEM);
            return decoder->last_error;
        }
        memcpy(decoder->context->extradata, extradata, extradata_size);
        decoder->context->extradata_size = (int)extradata_size;
    }

    const int ret = avcodec_open2(decoder->context, decoder->decoder, NULL);
    decoder->last_error = ret;
    return ret;
}

RuzuFfmpegHardwareContext* ruzu_ffmpeg_hardware_context_create(void) {
    return calloc(1, sizeof(RuzuFfmpegHardwareContext));
}

void ruzu_ffmpeg_hardware_context_destroy(RuzuFfmpegHardwareContext* hardware) {
    if (hardware == NULL) {
        return;
    }
    av_buffer_unref(&hardware->gpu_decoder);
    free(hardware);
}

int ruzu_ffmpeg_decoder_supports_decoding_on_device(uint64_t codec, int device_type, int* out_pix_fmt) {
    const AVCodec* decoder = ruzu_find_decoder(codec, 0);
    if (decoder == NULL) {
        return 0;
    }

    for (int i = 0;; i++) {
        const AVCodecHWConfig* config = avcodec_get_hw_config(decoder, i);
        if (config == NULL) {
            return 0;
        }
        if ((config->methods & AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX) != 0 &&
            config->device_type == (enum AVHWDeviceType)device_type) {
            if (out_pix_fmt != NULL) {
                *out_pix_fmt = config->pix_fmt;
            }
            return 1;
        }
    }
}

uintptr_t ruzu_ffmpeg_preferred_device_types(int* out, uintptr_t out_capacity) {
    const uintptr_t count = RUZU_PREFERRED_GPU_DECODER_COUNT;
    if (out != NULL) {
        const uintptr_t written = count < out_capacity ? count : out_capacity;
        for (uintptr_t index = 0; index < written; index++) {
            out[index] = RUZU_PREFERRED_GPU_DECODERS[index];
        }
    }
    return count;
}

const char* ruzu_ffmpeg_device_type_name(int device_type) {
    const char* name = av_hwdevice_get_type_name((enum AVHWDeviceType)device_type);
    return name != NULL ? name : "unknown";
}

uintptr_t ruzu_ffmpeg_supported_device_types(int* out, uintptr_t out_capacity) {
    uintptr_t count = 0;
    enum AVHWDeviceType current_device_type = AV_HWDEVICE_TYPE_NONE;

    while (1) {
        current_device_type = av_hwdevice_iterate_types(current_device_type);
        if (current_device_type == AV_HWDEVICE_TYPE_NONE) {
            return count;
        }

        if (out != NULL && count < out_capacity) {
            out[count] = current_device_type;
        }
        count++;
    }
}

int ruzu_ffmpeg_hardware_initialize_with_type(RuzuFfmpegHardwareContext* hardware,
                                              int device_type) {
    if (hardware == NULL) {
        return 0;
    }

    av_buffer_unref(&hardware->gpu_decoder);
    const enum AVHWDeviceType type = (enum AVHWDeviceType)device_type;
    hardware->last_error =
        av_hwdevice_ctx_create(&hardware->gpu_decoder, type, NULL, NULL, 0);
    if (hardware->last_error < 0) {
        return 0;
    }

#ifdef LIBVA_FOUND
    if (type == AV_HWDEVICE_TYPE_VAAPI) {
        AVHWDeviceContext* hwctx = (AVHWDeviceContext*)hardware->gpu_decoder->data;
        AVVAAPIDeviceContext* vactx = (AVVAAPIDeviceContext*)hwctx->hwctx;
        const char* vendor_name = vaQueryVendorString(vactx->display);
        if (vendor_name != NULL && strstr(vendor_name, "VDPAU backend") != NULL) {
            return -1;
        }
    }
#endif

    return 1;
}

int ruzu_ffmpeg_hardware_last_error(const RuzuFfmpegHardwareContext* hardware) {
    return hardware != NULL ? hardware->last_error : -1;
}

const char* ruzu_ffmpeg_hardware_vaapi_vendor_name(
    const RuzuFfmpegHardwareContext* hardware, int device_type) {
#ifdef LIBVA_FOUND
    if (hardware != NULL && hardware->gpu_decoder != NULL &&
        (enum AVHWDeviceType)device_type == AV_HWDEVICE_TYPE_VAAPI) {
        AVHWDeviceContext* hwctx = (AVHWDeviceContext*)hardware->gpu_decoder->data;
        AVVAAPIDeviceContext* vactx = (AVVAAPIDeviceContext*)hwctx->hwctx;
        return vaQueryVendorString(vactx->display);
    }
#else
    (void)hardware;
    (void)device_type;
#endif
    return NULL;
}

int ruzu_ffmpeg_decoder_initialize_hardware(RuzuFfmpegDecoder* decoder,
                                            const RuzuFfmpegHardwareContext* hardware,
                                            int pixel_format) {
    if (decoder == NULL || decoder->context == NULL || hardware == NULL ||
        hardware->gpu_decoder == NULL) {
        return 0;
    }
    av_buffer_unref(&decoder->context->hw_device_ctx);
    decoder->context->hw_device_ctx = av_buffer_ref(hardware->gpu_decoder);
    if (decoder->context->hw_device_ctx == NULL) {
        return 0;
    }
    decoder->context->get_format = ruzu_get_gpu_format;
    decoder->context->pix_fmt = (enum AVPixelFormat)pixel_format;
    return 1;
}

void ruzu_ffmpeg_decoder_destroy(RuzuFfmpegDecoder* decoder) {
    if (decoder == NULL) {
        return;
    }
    av_buffer_unref(&decoder->context->hw_device_ctx);
    avcodec_free_context(&decoder->context);
    free(decoder);
}

int ruzu_ffmpeg_decoder_send_packet(RuzuFfmpegDecoder* decoder, const uint8_t* data,
                                    uintptr_t size, int64_t pts, int64_t dts) {
    if (decoder == NULL || decoder->context == NULL) {
        return -1;
    }

    AVPacket* packet = av_packet_alloc();
    if (packet == NULL) {
        decoder->last_error = AVERROR(ENOMEM);
        return -1;
    }
    packet->data = (uint8_t*)data;
    packet->size = (int)size;
    packet->pts = pts;
    packet->dts = dts;

    const int ret = avcodec_send_packet(decoder->context, packet);
    decoder->last_error = ret;
    av_packet_free(&packet);
    return ret;
}

int ruzu_ffmpeg_decoder_last_error(const RuzuFfmpegDecoder* decoder) {
    if (decoder == NULL) {
        return -1;
    }
    return decoder->last_error;
}

int ruzu_ffmpeg_error_is_eof_or_again(int error) {
    return error == AVERROR_EOF || error == AVERROR(EAGAIN);
}

void ruzu_ffmpeg_error_string(int errnum, char* out, uintptr_t out_size) {
    if (out == NULL || out_size == 0) {
        return;
    }
    av_make_error_string(out, out_size, errnum);
}

AVFrame* ruzu_ffmpeg_decoder_receive_frame(RuzuFfmpegDecoder* decoder) {
    if (decoder == NULL || decoder->context == NULL) {
        return NULL;
    }

    AVFrame* frame = av_frame_alloc();
    if (frame == NULL) {
        return NULL;
    }

    const int ret = avcodec_receive_frame(decoder->context, frame);
    decoder->last_error = ret;
    if (ret < 0) {
        av_frame_free(&frame);
        return NULL;
    }

    return frame;
}

AVFrame* ruzu_ffmpeg_decoder_receive_frame_with_hw_transfer(RuzuFfmpegDecoder* decoder) {
    if (decoder == NULL || decoder->context == NULL) {
        return NULL;
    }

    if (decoder->context->hw_device_ctx == NULL) {
        return ruzu_ffmpeg_decoder_receive_frame(decoder);
    }

    AVFrame* intermediate = av_frame_alloc();
    AVFrame* output = av_frame_alloc();
    if (intermediate == NULL || output == NULL) {
        av_frame_free(&intermediate);
        av_frame_free(&output);
        return NULL;
    }

    int ret = avcodec_receive_frame(decoder->context, intermediate);
    decoder->last_error = ret;
    if (ret < 0) {
        av_frame_free(&intermediate);
        av_frame_free(&output);
        return NULL;
    }

    output->format = RUZU_PREFERRED_GPU_FORMAT;
    ret = av_hwframe_transfer_data(output, intermediate, 0);
    decoder->last_error = ret;
    av_frame_free(&intermediate);
    if (ret < 0) {
        av_frame_free(&output);
        return NULL;
    }

    return output;
}

AVFrame* ruzu_ffmpeg_frame_create(void) {
    return av_frame_alloc();
}

void ruzu_ffmpeg_frame_destroy(AVFrame* frame) {
    av_frame_free(&frame);
}

int ruzu_ffmpeg_frame_width(const AVFrame* frame) {
    return frame != NULL ? frame->width : 0;
}

int ruzu_ffmpeg_frame_height(const AVFrame* frame) {
    return frame != NULL ? frame->height : 0;
}

int ruzu_ffmpeg_frame_format(const AVFrame* frame) {
    return frame != NULL ? frame->format : -1;
}

int ruzu_ffmpeg_frame_stride(const AVFrame* frame, int plane) {
    if (frame == NULL || plane < 0 || plane >= AV_NUM_DATA_POINTERS) {
        return 0;
    }
    return frame->linesize[plane];
}

const uint8_t* ruzu_ffmpeg_frame_plane(const AVFrame* frame, int plane) {
    if (frame == NULL || plane < 0 || plane >= AV_NUM_DATA_POINTERS) {
        return NULL;
    }
    return frame->data[plane];
}

int ruzu_ffmpeg_frame_interlaced(const AVFrame* frame) {
    if (frame == NULL) {
        return 0;
    }
#if defined(FF_API_INTERLACED_FRAME) || LIBAVUTIL_VERSION_MAJOR >= 59
    return (frame->flags & AV_FRAME_FLAG_INTERLACED) != 0;
#else
    return frame->interlaced_frame != 0;
#endif
}

int ruzu_ffmpeg_frame_is_hardware_decoded(const AVFrame* frame) {
    return frame != NULL && frame->hw_frames_ctx != NULL;
}
