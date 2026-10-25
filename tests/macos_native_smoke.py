"""Exercise the real native ABI with a generated, non-private LinearRaw DNG.

macOS CI runs this against the packaged dylib. It can also validate the same
fixture on Windows (whose native path ABI uses UTF-16). No camera files, NumPy,
GUI, or Adobe installation are required; this is not camera/GUI coverage.
"""
import ctypes
from datetime import date
import hashlib
import math
import os
from pathlib import Path
import struct
import sys
import tempfile


class Info(ctypes.Structure):
    _fields_ = [(name, ctypes.c_uint32) for name in ("abi", "w", "h", "orientation", "pw", "ph")] + [
        (name, ctypes.c_float) for name in ("iso", "aperture", "shutter", "focal")] + [
        ("make", ctypes.c_char * 64), ("model", ctypes.c_char * 64),
        ("lens", ctypes.c_char * 128), ("captured", ctypes.c_char * 32)]


Cancel = ctypes.CFUNCTYPE(ctypes.c_int, ctypes.c_void_p)


class Frame(ctypes.Structure):
    _fields_ = [(name, ctypes.c_uint32) for name in
                ("kind", "width", "height", "channels", "bits", "rendered")] + [
        ("length", ctypes.c_uint64), ("data", ctypes.c_void_p), ("owner", ctypes.c_void_p)]


class DevelopParams(ctypes.Structure):
    _fields_ = [("abi", ctypes.c_uint32), ("max_edge", ctypes.c_uint32)] + [
        (name, ctypes.c_double) for name in ("temperature", "tint", "exposure")] + [
        ("cancelled", Cancel), ("context", ctypes.c_void_p)]


class DevelopInfo(ctypes.Structure):
    _fields_ = [("abi", ctypes.c_uint32), ("orientation", ctypes.c_uint32),
                ("camera_mul", ctypes.c_float * 4), ("applied_mul", ctypes.c_float * 4),
                ("camera_xyz", ctypes.c_float * 12)] + [
        (name, ctypes.c_double) for name in
        ("temperature", "tint", "open_ms", "unpack_ms", "process_ms")]


def write_linear_dng(path, width=128, height=96):
    """Write one uncompressed RGB16 strip, a D65 matrix, and camera WB.

    TIFF/DNG tags deliberately contain no thumbnail: successful bitmap output
    must come from developing the linear sensor samples, not a JPEG fallback.
    The DNG matrix is synthetic XYZ-to-camera identity. A supported Sony model
    identifier supplies LibRaw's separate camera_xyz lookup for custom WB;
    no pixels or metadata were copied from a photo. This does not test unknown
    cameras, camera accuracy, or complete DNG profile compatibility.
    """
    entries = []

    def tag(number, kind, values):
        if kind == 2:
            data = values.encode("ascii") + b"\0"
            count = len(data)
        elif kind in (5, 10):
            count = len(values)
            data = b"".join(struct.pack("<ii" if kind == 10 else "<II", *v)
                            for v in values)
        else:
            count = len(values)
            data = struct.pack("<" + {1: "B", 3: "H", 4: "I"}[kind] * count, *values)
        entries.append((number, kind, count, data))

    tag(254, 4, [0])  # NewSubFileType: raw image
    tag(256, 4, [width])
    tag(257, 4, [height])
    tag(258, 3, [16, 16, 16])
    tag(259, 3, [1])  # uncompressed
    tag(262, 3, [34892])  # LinearRaw, in camera-native color space
    tag(271, 2, "SONY")
    tag(272, 2, "ILCE-7M4")
    tag(273, 4, [0])  # StripOffsets: patched after the IFD data
    tag(274, 3, [1])
    tag(277, 3, [3])
    tag(278, 4, [height])
    tag(279, 4, [width * height * 6])
    tag(284, 3, [1])
    tag(50706, 1, [1, 4, 0, 0])
    tag(50707, 1, [1, 1, 0, 0])
    tag(50708, 2, "FastCull Synthetic LinearRaw ABI Fixture")
    tag(50714, 5, [(0, 1)])
    tag(50717, 4, [65535])
    tag(50721, 10, [(int(row == col), 1) for row in range(3) for col in range(3)])
    tag(50728, 5, [(1, 2), (1, 1), (3, 4)])  # AsShotNeutral
    tag(50778, 3, [21])  # CalibrationIlluminant1: D65
    entries.sort()
    offset = 8 + 2 + len(entries) * 12 + 4
    payload = bytearray()
    fields = []
    for number, kind, count, data in entries:
        if len(data) <= 4:
            value = data.ljust(4, b"\0")
        else:
            value = struct.pack("<I", offset + len(payload))
            payload.extend(data)
            if len(payload) % 2:
                payload.append(0)
        fields.append((number, struct.pack("<HHI", number, kind, count) + value))
    image_offset = offset + len(payload)
    header = b"II" + struct.pack("<HIH", 42, 8, len(entries))
    for number, field in fields:
        if number == 273:
            field = field[:8] + struct.pack("<I", image_offset)
        header += field
    pixels = bytearray()
    for y in range(height):
        for x in range(width):
            pixels.extend(struct.pack("<HHH", 1500 + x * 65, 2000 + y * 70,
                                      1800 + (x + y) * 33))
    path.write_bytes(header + struct.pack("<I", 0) + payload + pixels)
    return width, height


def bind_library(library):
    lib = ctypes.CDLL(str(library))
    path_type = ctypes.c_wchar_p if os.name == "nt" else ctypes.c_char_p
    lib.fc_raw_info.argtypes = [path_type, ctypes.POINTER(Info), ctypes.c_void_p]
    lib.fc_raw_info.restype = ctypes.c_int
    lib.fc_raw_decode.argtypes = [path_type, ctypes.c_int, ctypes.POINTER(Frame), ctypes.c_void_p]
    lib.fc_raw_decode.restype = ctypes.c_int
    lib.fc_raw_release.argtypes = [ctypes.c_void_p]
    lib.fc_raw_release.restype = None
    lib.fc_raw_develop.argtypes = [path_type, ctypes.POINTER(DevelopParams),
                                  ctypes.POINTER(Frame), ctypes.POINTER(DevelopInfo), ctypes.c_void_p]
    lib.fc_raw_develop.restype = ctypes.c_int
    lib.fc_raw_develop_cancel.argtypes = []
    lib.fc_raw_develop_cancel.restype = None
    return lib


def native_path(path):
    return str(path) if os.name == "nt" else os.fsencode(path)


def run_smoke(lib, root):
    # x86_64 and arm64 have the same 64-bit C layout; fail before native writes
    # if a Python ABI or accidental field edit would make buffers too short.
    assert ctypes.sizeof(ctypes.c_void_p) == 8, "64-bit native adapter required"
    assert ctypes.sizeof(Info) == 328
    assert ctypes.sizeof(Frame) == 48
    assert ctypes.sizeof(DevelopParams) == 48
    assert ctypes.sizeof(DevelopInfo) == 128
    missing = native_path(root / "不存在 中文 space'$.ARW")
    for entry in ("info", "decode", "develop"):
        error = ctypes.create_string_buffer(256)
        frame = Frame()
        if entry == "info":
            code = lib.fc_raw_info(missing, ctypes.byref(Info()), error)
        elif entry == "decode":
            code = lib.fc_raw_decode(missing, 0, ctypes.byref(frame), error)
        else:
            params = DevelopParams(1, 128, 0, 0, 0, Cancel(), None)
            code = lib.fc_raw_develop(missing, ctypes.byref(params), ctypes.byref(frame),
                                      ctypes.byref(DevelopInfo()), error)
        assert code != 0 and error.value, f"{entry}: missing UTF path needs diagnostic"
        assert not frame.owner and not frame.data, f"{entry}: failure published an owned frame"
    lib.fc_raw_release(None)
    lib.fc_raw_develop_cancel()  # no active request is a safe no-op

    fixture = root / "线性 DNG space'$.dng"
    dimensions = write_linear_dng(fixture)
    source_hash = hashlib.sha256(fixture.read_bytes()).digest()
    metadata = Info()
    error = ctypes.create_string_buffer(256)
    assert lib.fc_raw_info(native_path(fixture), ctypes.byref(metadata), error) == 0, error.value
    assert metadata.abi == 1 and (metadata.w, metadata.h) == dimensions
    assert metadata.orientation == 1

    def develop(label, temperature=0, tint=0, exposure=0, callback=None, expect_failure=False,
                result_info=None, source=None):
        info = result_info if result_info is not None else DevelopInfo()
        frame = Frame()
        error = ctypes.create_string_buffer(256)
        # Keep callback alive until the synchronous native call has returned.
        cancellation = callback if callback is not None else Cancel()
        params = DevelopParams(1, 128, temperature, tint, exposure, cancellation, None)
        code = lib.fc_raw_develop(native_path(source or fixture), ctypes.byref(params), ctypes.byref(frame),
                                  ctypes.byref(info), error)
        try:
            if expect_failure:
                assert code != 0 and error.value, f"{label}: cancellation must fail with diagnostic"
                assert not frame.owner and not frame.data and frame.length == 0, label
                print(f"{label}: cancelled, {error.value.decode(errors='replace')}")
                return None, info
            assert code == 0, f"{label}: {error.value.decode(errors='replace')}"
            assert info.abi == 1 and info.orientation == 1, label
            assert (frame.kind, frame.channels, frame.bits, frame.rendered) == (2, 3, 8, 1), label
            assert (frame.width, frame.height) == dimensions, label
            assert frame.length == frame.width * frame.height * 3, label
            assert frame.owner and frame.data, f"{label}: missing owned RGB buffer"
            assert all(math.isfinite(value) and value >= 0 for value in
                       (info.open_ms, info.unpack_ms, info.process_ms)), label
            pixels = ctypes.string_at(frame.data, frame.length)
            assert max(pixels) - min(pixels) > 20, f"{label}: empty/flat bitmap"
            print(f"{label}: RGB8 {frame.width}x{frame.height}, "
                  f"unpack_ms={info.unpack_ms:.6f}, sha256={hashlib.sha256(pixels).hexdigest()}")
            return pixels, info
        finally:
            if frame.owner:
                lib.fc_raw_release(frame.owner)

    baseline, first = develop("camera")
    assert first.unpack_ms > 0, "first call must unpack the synthetic RAW"
    assert all(
        abs(actual - expected) < 1e-5 for actual, expected in
        zip(first.camera_mul, (2, 1, 4 / 3))), "AsShotNeutral not read"
    bright, cached = develop("exposure+1", exposure=1)
    assert cached.unpack_ms == 0, "repeated call must reuse unpacked RAW"
    assert sum(bright) > sum(baseline) * 1.15, "linear exposure did not brighten RGB output"
    custom, cached = develop("custom-WB", 7500, 20)
    assert cached.unpack_ms == 0 and custom != baseline, "custom WB had no pixel effect"
    assert cached.temperature == 7500 and cached.tint == 20
    assert all(value > 0 and math.isfinite(value) for value in cached.applied_mul)
    assert list(cached.applied_mul) != list(cached.camera_mul), "custom WB ignored"
    restored, cached = develop("camera-restored")
    assert restored == baseline and cached.unpack_ms == 0, "EV/WB leaked into cached camera state"
    repeated, cached = develop("custom-WB-repeated", 7500, 20)
    assert repeated == custom and cached.unpack_ms == 0, "cached custom WB was not deterministic"

    cancellation_seen = []
    callback_info = DevelopInfo()

    @Cancel
    def cancel_callback(_):
        # Cancel after the active instance exists on both platforms, rather
        # than during Windows' earlier mutex polling (which preserves cache).
        if callback_info.abi == 1:
            cancellation_seen.append(True)
            return 1
        return 0

    develop("callback-cancel", callback=cancel_callback, expect_failure=True, result_info=callback_info)
    assert cancellation_seen, "native cancellation callback not called"
    recovered, recovery = develop("callback-recovered")
    assert recovered == baseline and recovery.unpack_ms > 0, "cancelled cache was not reset"

    active_info = DevelopInfo()
    export_cancel_seen = []
    cancel_fixture = root / "export-cancel.dng"
    write_linear_dng(cancel_fixture)

    @Cancel
    def cancel_active(_):
        # Signal after the instance is active but before unpack, where the
        # DNG loader checks LibRaw's native flag. Use a fresh path to exercise
        # decoding, not cached linear processing (which uses our callback).
        # Windows' earlier mutex polls have info.abi == 0.
        if active_info.abi == 1 and not export_cancel_seen:
            export_cancel_seen.append(True)
            lib.fc_raw_develop_cancel()
        return 0

    develop("export-cancel", callback=cancel_active, expect_failure=True,
            result_info=active_info, source=cancel_fixture)
    assert export_cancel_seen, "fc_raw_develop_cancel was not invoked during an active request"
    recovered, recovery = develop("export-recovered")
    assert recovered == baseline and recovery.unpack_ms > 0, "export cancellation did not recover"
    lib.fc_raw_develop_cancel()
    recovered, cached = develop("idle-cancel-safe")
    assert recovered == baseline and cached.unpack_ms == 0, "idle cancellation poisoned the next request"
    assert hashlib.sha256(fixture.read_bytes()).digest() == source_hash, "native rendering edited the DNG"


def main():
    library = Path(sys.argv[1] if len(sys.argv) > 1 else os.environ["FASTCULL_RAW_DLL"]).resolve()
    lib = bind_library(library)
    scratch = Path(os.environ.get("FASTCULL_NATIVE_SMOKE_TEMP") or
                   Path(__file__).resolve().parents[1] / ".agent-temp" /
                   f"{date.today().isoformat()}-native-smoke")
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="fastcull-native-",
                                     dir=scratch) as temporary:
        run_smoke(lib, Path(temporary))
    print(f"{sys.platform} native adapter: real RGB develop, EV/WB/cache, cancellation and release ABI passed")


if __name__ == "__main__":
    main()
