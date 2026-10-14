"""Run the actual dylib ABI on a Mac. No photo edits; not a camera coverage test."""
import ctypes
from pathlib import Path
import sys


class Info(ctypes.Structure):
    _fields_ = [(name, ctypes.c_uint32) for name in ("abi", "w", "h", "orientation", "pw", "ph")] + [
        (name, ctypes.c_float) for name in ("iso", "aperture", "shutter", "focal")] + [
        ("make", ctypes.c_char * 64), ("model", ctypes.c_char * 64),
        ("lens", ctypes.c_char * 128), ("captured", ctypes.c_char * 32)]


def main():
    lib = ctypes.CDLL(str(Path(sys.argv[1]).resolve()))
    lib.fc_raw_info.argtypes = [ctypes.c_char_p, ctypes.POINTER(Info), ctypes.c_void_p]
    lib.fc_raw_info.restype = ctypes.c_int
    lib.fc_raw_release.argtypes = [ctypes.c_void_p]
    lib.fc_raw_release.restype = None
    error = ctypes.create_string_buffer(256)
    assert lib.fc_raw_info('/不存在/中文 space\'$.ARW'.encode(), ctypes.byref(Info()), error) != 0
    assert error.value, "native errors must include a diagnostic"
    lib.fc_raw_release(None)
    print("macOS dylib: loaded, UTF-8 error path and release ABI passed")


if __name__ == "__main__":
    main()
