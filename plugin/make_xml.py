#!/usr/bin/env python3
"""Build plugin/ma3-osc.xml (importable grandMA3 plugin library) from the .lua files.

Run after editing ma3-osc.lua or SettingsPage.lua:  python3 plugin/make_xml.py
"""
import base64
import pathlib

HERE = pathlib.Path(__file__).resolve().parent
PLUGINS = [
    # (name, plugin guid, component guid, source)
    ("MA3-OSC Start Stop", "C1 19 BD 33 A9 FD 10 03 5E 76 48 94 2C 0E 91 01",
     "C1 19 BD 33 96 FC 10 02 7D 74 11 B4 F6 65 C9 01", "ma3-osc.lua"),
    ("MA3-OSC Settings", "C1 19 BD 33 A9 FD 10 03 5E 76 48 94 2C 0E 91 02",
     "C1 19 BD 33 7E A6 10 02 EB 81 42 CE E9 06 2B 02", "SettingsPage.lua"),
]
BLOCK = 1024


def plugin_xml(name, guid, comp_guid, source):
    data = (HERE / source).read_bytes()
    data.decode("ascii")  # grandMA3 blocks are split on bytes: keep sources plain ASCII
    blocks = [data[i:i + BLOCK] for i in range(0, len(data), BLOCK)]
    lines = [
        f'    <UserPlugin Name="{name}" Guid="{guid}" Version="1.0.0.0">',
        f'        <ComponentLua Guid="{comp_guid}">',
        f'            <FileContent Size="{len(blocks)}">',
    ]
    lines += [f'                <Block Base64="{base64.b64encode(b).decode()}"/>' for b in blocks]
    lines += ["            </FileContent>", "        </ComponentLua>", "    </UserPlugin>"]
    return "\n".join(lines)


out = ['<?xml version="1.0" encoding="UTF-8"?>', '<GMA3 DataVersion="2.0.2.0">']
out += [plugin_xml(*p) for p in PLUGINS]
out.append("</GMA3>\n")
(HERE / "ma3-osc.xml").write_text("\n".join(out))
print("wrote", HERE / "ma3-osc.xml")
