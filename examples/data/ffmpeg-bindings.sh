#!/bin/bash
# bindgen runs of examples/data/ffmpeg-wrapper.h for the three hosts, then
# the generator. Wants a directory holding only ffmpeg's headers (a copy of
# /usr/include/libav*) in FFINC, and zig's libc headers for the other hosts
set -e
: "${FFINC:?set FFINC to a directory holding only ffmpeg's headers}"
: "${ZIG_LIBC:?set ZIG_LIBC to zig's lib/libc/include}"
Z=$ZIG_LIBC
cd "$(dirname "$0")"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
FUNS='av_version_info|av_log_set_level|av_log_set_callback|av_log_default_callback|av_log_format_line2|av_log|avcodec_get_name|av_packet_alloc|av_packet_free|av_packet_unref|av_frame_alloc|av_frame_free|av_frame_unref|av_frame_get_buffer|avcodec_find_decoder|avcodec_find_encoder_by_name|avformat_version|avformat_open_input|avformat_find_stream_info|av_read_frame|avformat_close_input|avformat_alloc_context|avcodec_version|avcodec_alloc_context3|avcodec_free_context|avcodec_parameters_to_context|avcodec_open2|avcodec_send_packet|avcodec_receive_frame|avcodec_send_frame|avcodec_receive_packet|avio_alloc_context|avio_context_free|av_malloc|av_free|av_strerror|av_image_alloc|av_dict_set|av_dict_free'
TYPES='AVFormatContext|AVCodecContext|AVPacket|AVFrame|AVIOContext|AVCodec|AVCodecParameters|AVStream|AVMediaType|AVCodecID|AVPixelFormat|AVDictionary|AVInputFormat|AVRational'
VARS='AV_LOG_.*|FF_THREAD_.*|AVIO_FLAG_.*|AVSEEK_SIZE|AV_PIX_FMT_.*|AVERROR_EOF|LIBAVCODEC_VERSION_MAJOR|LIBAVFORMAT_VERSION_MAJOR'
CF="-I$FFINC"
run() { out=$1; shift; bindgen ffmpeg-wrapper.h -o "$out" --allowlist-function "^($FUNS)\$" --allowlist-type "^($TYPES)\$" --allowlist-var "^($VARS)\$" --no-layout-tests --no-doc-comments -- "$@" $CF; }
run "$tmp/lin.rs"
run "$tmp/win.rs" --target=x86_64-pc-windows-msvc -fgnuc-version=4.2.1 -isystem $Z/any-windows-any -Wno-dll-attribute-on-redeclaration
run "$tmp/mac.rs" --target=aarch64-apple-macos -isystem $Z/any-darwin-any
(cd ../.. && cargo xtask host-bindings "$tmp/lin.rs" --windows-input "$tmp/win.rs" --macos-input "$tmp/mac.rs" --name FFMPEG \
  --linux libavformat.so.63 --linux libavformat.so.62 --linux libavformat.so.61 --linux libavformat.so.60 \
  --macos /opt/homebrew/lib/libavformat.dylib --macos /usr/local/lib/libavformat.dylib \
  --windows avformat-63.dll --windows avformat-62.dll --windows avformat-61.dll --bsd libavformat.so \
  -o examples/src/hostlibs/ffmpeg.rs)
