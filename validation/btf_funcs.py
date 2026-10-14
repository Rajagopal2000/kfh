"""Prints the name of every BTF FUNC in a vmlinux ELF file, one per line."""
import struct
import sys

data = open(sys.argv[1], "rb").read()
shoff, = struct.unpack_from("<Q", data, 0x28)
shentsize, shnum, shstrndx = struct.unpack_from("<HHH", data, 0x3A)
sections = []
for i in range(shnum):
    name, typ, flags, addr, off, size = struct.unpack_from("<IIQQQQ", data, shoff + i * shentsize)
    sections.append((name, off, size))
strtab_off = sections[shstrndx][1]
def sec_name(n):
    end = data.index(b"\0", strtab_off + n)
    return data[strtab_off + n:end].decode()
btf = next(data[off:off + size] for name, off, size in sections if sec_name(name) == ".BTF")

magic, version, flags, hdr_len, type_off, type_len, str_off, str_len = struct.unpack_from("<HBBIIIII", btf, 0)
assert magic == 0xEB9F, hex(magic)
types = btf[hdr_len + type_off: hdr_len + type_off + type_len]
strings = btf[hdr_len + str_off: hdr_len + str_off + str_len]
def string(off):
    return strings[off:strings.index(b"\0", off)].decode()

extra = {1: lambda v: 4, 3: lambda v: 12, 4: lambda v: 12 * v, 5: lambda v: 12 * v, 6: lambda v: 8 * v,
         13: lambda v: 8 * v, 14: lambda v: 4, 15: lambda v: 12 * v, 17: lambda v: 4, 19: lambda v: 12 * v}
pos = 0
while pos < len(types):
    name_off, info, _ = struct.unpack_from("<III", types, pos)
    kind = (info >> 24) & 0x1F
    vlen = info & 0xFFFF
    pos += 12 + extra.get(kind, lambda v: 0)(vlen)
    if kind == 12:
        print(string(name_off))
