import ctypes, struct, sys, platform
libm = ctypes.CDLL("libm.so.6")
libm.pow.restype = ctypes.c_double; libm.pow.argtypes = [ctypes.c_double, ctypes.c_double]
libc = ctypes.CDLL("libc.so.6"); libc.gnu_get_libc_version.restype = ctypes.c_char_p
sys.stderr.write("glibc %s %s\n" % (libc.gnu_get_libc_version().decode(), platform.machine()))
for m in range(1, 4033):
    x = float(m) / 28.2
    p = libm.pow(x, -1.228)
    print(m, "%016x" % struct.unpack("<Q", struct.pack("<d", p))[0])
