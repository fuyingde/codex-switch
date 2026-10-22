"""Inspect the PE imports without executing the EXE. No third-party packages needed."""
from pathlib import Path
import struct
import sys


def imports(path):
    data = Path(path).read_bytes()
    u16 = lambda offset: struct.unpack_from("<H", data, offset)[0]
    u32 = lambda offset: struct.unpack_from("<I", data, offset)[0]
    assert data[:2] == b"MZ", "not a Windows executable"
    pe = u32(0x3C)
    assert data[pe:pe+4] == b"PE\0\0", "invalid PE signature"
    optional = pe + 24
    assert u16(optional) == 0x20B, "expected a 64-bit PE"
    directory = optional + 112
    section_table = optional + u16(pe + 20)
    sections = []
    for i in range(u16(pe + 6)):
        offset = section_table + 40 * i
        sections.append((u32(offset+12), max(u32(offset+8), u32(offset+16)), u32(offset+20)))

    def file_offset(rva):
        if rva < u32(optional+60):
            return rva
        for address, size, start in sections:
            if address <= rva < address + size:
                return start + rva - address
        raise ValueError(f"unmapped RVA {rva:x}")

    def string(rva):
        offset = file_offset(rva)
        return data[offset:data.index(0, offset)].decode("ascii").lower()

    result = set()
    table = u32(directory+8)
    if table:
        offset = file_offset(table)
        while any(data[offset:offset+20]):
            result.add(string(u32(offset+12)))
            offset += 20
    # Also audit delay-loaded DLLs; normal imports alone cannot prove portability.
    delayed = u32(directory+13*8)
    if delayed:
        offset = file_offset(delayed)
        while any(data[offset:offset+32]):
            if u32(offset) & 1 == 0:
                raise ValueError("unsupported VA-based delay import table")
            result.add(string(u32(offset+4)))
            offset += 32
    return result


if __name__ == "__main__":
    modules = imports(sys.argv[1])
    forbidden = [name for name in modules if name.startswith(("vcruntime", "msvcp", "concrt"))
                 or any(word in name for word in ("webview", "libcef", "coreclr", "hostfxr", "d3dcompiler_"))]
    assert not forbidden, f"unexpected external runtime dependencies: {forbidden}"
    assert {"d2d1.dll", "dwrite.dll"} <= modules, "expected native Direct2D/DirectWrite imports"
    print("PASS: native graphics; no Visual C++, .NET, WebView or legacy D3D compiler DLL requirement")
    print("Direct imports:", ", ".join(sorted(modules)))
