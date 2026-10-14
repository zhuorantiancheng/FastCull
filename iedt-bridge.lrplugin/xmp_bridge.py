# -*- coding: utf-8 -*-
"""ARW 内嵌 XMP 桥接工具: 在 Sony Imaging Edge Viewer 与 Lightroom/ACR 之间
双向同步评分与颜色标签。

正向 (--apply):   LR/ACR 旁车(.xmp) -> ARW 内嵌 XMP 包(TIFF tag 0x02BC),
                  让 Viewer 的"评价"显示 LR 打的星。
反向 (--reverse): ARW 内嵌 XMP -> 旁车文件, 让 LR 读到 Viewer 打的星
                  (已有旁车只改 Rating/Label 字段, 不动 develop 设置;
                   无旁车则创建最小旁车)。

原理: Viewer 把星级存放在 ARW 内嵌 XMP 的 <xmp:Rating>N</xmp:Rating>;
LR/ACR 则写旁车文件。正向重建内嵌包时与原包字节长度完全一致(空白
padding 补齐), 不移动任何 TIFF 偏移, 文件结构与 Sony 解析器完全兼容。

用法:
  python xmp_bridge.py --dry-run <文件夹>     # 预览正向将要修改的文件
  python xmp_bridge.py --apply <文件夹>       # 正向: 旁车 -> ARW 内嵌
  python xmp_bridge.py --reverse <文件夹>     # 反向: ARW 内嵌 -> 旁车
  python xmp_bridge.py --apply <文件1> <文件2> # 指定文件
"""
from __future__ import annotations

import argparse
import os
import re
import struct
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

RATING_RE = re.compile(rb"<xmp:Rating>(\d)</xmp:Rating>")
LABEL_RE = re.compile(rb"<xmp:Label>([^<]*)</xmp:Label>")

PACKET_TMPL = (
    "<?xpacket begin='\ufeff' id='W5M0MpCehiHzreSzNTczkc9d'?>\n"
    "<x:xmpmeta xmlns:x='adobe:ns:meta/' x:xmptk=''>\n"
    "<rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'>\n"
    " <rdf:Description rdf:about=''\n"
    "  xmlns:xmp='http://ns.adobe.com/xap/1.0/'>\n"
    "  <xmp:Rating>{rating}</xmp:Rating>\n"
    "{label_line}"
    " </rdf:Description>\n"
    "</rdf:RDF>\n"
    "</x:xmpmeta>\n"
)


def find_embedded_xmp(raw: bytes):
    """返回 (offset, count) 或 None。仅解析 IFD0 的 tag 700。"""
    if raw[:2] != b"II":
        return None
    ifd0 = struct.unpack_from("<I", raw, 4)[0]
    n = struct.unpack_from("<H", raw, ifd0)[0]
    for i in range(n):
        e = ifd0 + 2 + i * 12
        tag, typ, cnt = struct.unpack_from("<HHI", raw, e)
        if tag == 700:
            off = struct.unpack_from("<I", raw, e + 8)[0]
            return off, cnt
    return None


def read_sidecar(arw: Path):
    """读旁车 -> (rating:int|None, label:str|None)。兼容属性式/元素式。"""
    sc = arw.with_suffix(".xmp")
    if not sc.exists():
        sc = arw.with_suffix(".XMP")
    if not sc.exists():
        return None, None
    try:
        root = ET.parse(sc).getroot()
    except (ET.ParseError, OSError):
        return None, None
    rating = label = None
    for desc in root.iter("{http://www.w3.org/1999/02/22-rdf-syntax-ns#}Description"):
        for key, value in desc.attrib.items():
            if key.endswith("}Rating") or key == "Rating":
                try:
                    rating = max(0, min(5, int(value)))
                except ValueError:
                    pass
            if key.endswith("}Label") or key == "Label":
                label = value or None
        for child in desc:
            name = child.tag.rsplit("}", 1)[-1]
            text = (child.text or "").strip()
            if name == "Rating" and rating is None:
                try:
                    rating = max(0, min(5, int(text)))
                except ValueError:
                    pass
            if name == "Label" and label is None:
                label = text or None
    return rating, label


def build_packet(rating, label, total_len: int) -> bytes:
    """按 Viewer 模板重建 XMP 包, 总长精确等于 total_len (全部按字节计算)。"""
    label_line = f"  <xmp:Label>{label}</xmp:Label>\n" if label else ""
    body = PACKET_TMPL.format(rating=rating, label_line=label_line).encode("utf-8")
    end = b"<?xpacket end='w'?>"
    # 尾部 padding 在 </x:xmpmeta> 与 end 标记之间, 保持总长不变
    pad = total_len - len(body) - len(end)
    if pad < 0:
        raise ValueError(f"XMP 包空间不足: 需 {len(body)+len(end)}, 有 {total_len}")
    packet = body + b" " * pad + end
    assert len(packet) == total_len
    return packet


def patch_arw(arw: Path, apply: bool, rating=None, label=None) -> str:
    """返回状态字符串。apply=False 只预览。
    只读写文件头部的 XMP 包区域(通常 4KB), 不重写整个 RAW。
    rating/label 由调用方直接给定(LR 插件模式); 缺省时从旁车读取。"""
    if rating is None and label is None:
        rating, label = read_sidecar(arw)
    if rating is None and label is None:
        return "skip(无旁车数据)"
    with open(arw, "r+b") as f:
        head = f.read(4096)
        loc = find_embedded_xmp(head)
        if loc is None:
            # 头部 4KB 内没有 tag 700 的话再全量确认一次
            raw_all = arw.read_bytes()
            loc = find_embedded_xmp(raw_all)
            if loc is None:
                return "skip(ARW无内嵌XMP包)"
            off, cnt = loc
            packet = raw_all[off:off + cnt]
            write_from = None
        else:
            off, cnt = loc
            f.seek(off)
            packet = f.read(cnt)
            write_from = off
        if b"<?xpacket end=" not in packet:
            return "skip(内嵌包损坏)"
        m = RATING_RE.search(packet)
        cur_rating = int(m.group(1)) if m else None
        lm = LABEL_RE.search(packet)
        cur_label = lm.group(1).decode() if lm else None
        if rating == cur_rating and label == cur_label:
            return "keep(已一致)"
        new = build_packet(rating, label, cnt)
        if apply:
            if write_from is None:
                raw_all = bytearray(raw_all)
                raw_all[off:off + cnt] = new
                arw.write_bytes(bytes(raw_all))
            else:
                f.seek(write_from)
                f.write(new)
            return f"patched(rating {cur_rating}->{rating}, label {cur_label!r}->{label!r})"
        return f"would(rating {cur_rating}->{rating}, label {cur_label!r}->{label!r})"


def read_embedded(arw: Path):
    """读 ARW 内嵌包 -> (rating:int|None, label:str|None)。"""
    with open(arw, "rb") as f:
        head = f.read(4096)
    loc = find_embedded_xmp(head)
    if loc is None:
        raw_all = arw.read_bytes()
        loc = find_embedded_xmp(raw_all)
        if loc is None:
            return None, None
        packet = raw_all[loc[0]:loc[0] + loc[1]]
    else:
        packet = head[loc[0]:loc[0] + loc[1]]
    m = RATING_RE.search(packet)
    lm = LABEL_RE.search(packet)
    rating = int(m.group(1)) if m else None
    label = lm.group(1).decode() if lm else None
    return rating, label


# ---- 反向: ARW 内嵌 -> 旁车 -------------------------------------------------

SC_RATING_ATTR = re.compile(r"(xmp:Rating=')\d(')")
SC_RATING_ELEM = re.compile(r"(<xmp:Rating>)\d+(</xmp:Rating>)")
SC_LABEL_ATTR = re.compile(r"(xmp:Label=')[^']*(')")
SC_LABEL_ELEM = re.compile(r"(<xmp:Label>)[^<]*(</xmp:Label>)")

MINIMAL_SIDECAR = (
    "<?xpacket begin='\ufeff' id='W5M0MpCehiHzreSzNTczkc9d'?>\n"
    "<x:xmpmeta xmlns:x='adobe:ns:meta/' x:xmptk='photocheck-bridge'>\n"
    " <rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'>\n"
    "  <rdf:Description rdf:about=''\n"
    "    xmlns:xmp='http://ns.adobe.com/xap/1.0/'\n"
    "   {attrs}/>\n"
    " </rdf:RDF>\n"
    "</x:xmpmeta>\n"
    "<?xpacket end='w'?>\n"
)


def _set_sidecar_value(text: str, name: str, value) -> str:
    """在旁车 XML 文本里把 xmp:NAME 设为 value; 已有则原位替换(属性式或
    元素式), 没有则在 </rdf:RDF> 前插入一个自闭合 Description。只动这一个
    字段, develop 设置等其他内容保持不变。"""
    if value is None:
        return text
    sval = str(value)
    attr_re = re.compile(r"(xmp:%s=')[^']*(')" % name)
    elem_re = re.compile(r"(<xmp:%s>)[^<]*(</xmp:%s>)" % (name, name))
    if attr_re.search(text):
        return attr_re.sub(lambda m: m.group(1) + sval + m.group(2), text, count=1)
    if elem_re.search(text):
        return elem_re.sub(lambda m: m.group(1) + sval + m.group(2), text, count=1)
    block = (
        "  <rdf:Description rdf:about=''\n"
        "    xmlns:xmp='http://ns.adobe.com/xap/1.0/'\n"
        "   xmp:%s='%s'/>\n" % (name, sval)
    )
    return text.replace("</rdf:RDF>", block + "</rdf:RDF>", 1)


def reverse_arw(arw: Path) -> str:
    """ARW 内嵌评分/标签 -> 旁车。返回状态字符串。"""
    rating, label = read_embedded(arw)
    if rating is None and label is None:
        return "skip(内嵌无评分)"
    sidecar = arw.with_suffix(".xmp")
    if not sidecar.exists():
        attrs = []
        if rating is not None:
            attrs.append(f"xmp:Rating='{rating}'")
        if label:
            attrs.append(f"xmp:Label='{label}'")
        sidecar.write_text(
            MINIMAL_SIDECAR.format(attrs="\n    ".join(attrs)),
            encoding="utf-8",
        )
        return f"created(rating={rating}, label={label!r})"
    text = sidecar.read_text(encoding="utf-8", errors="strict")
    old = text
    cur_r = SC_RATING_ATTR.search(text) or SC_RATING_ELEM.search(text)
    cur_rating = None
    if cur_r:
        digits = re.search(r"\d+", cur_r.group(0))
        cur_rating = int(digits.group()) if digits else None
    if rating is not None and rating != cur_rating:
        text = _set_sidecar_value(text, "Rating", rating)
    cur_l = SC_LABEL_ATTR.search(text) or SC_LABEL_ELEM.search(text)
    cur_label = None
    if cur_l:
        m = re.search(r"Label.='?([^'<>]*)", cur_l.group(0))
        cur_label = m.group(1) if m else None
    if label and label != cur_label:
        text = _set_sidecar_value(text, "Label", label)
    if text == old:
        return "keep(已一致)"
    sidecar.write_text(text, encoding="utf-8")
    return f"updated(rating {cur_rating}->{rating}, label {cur_label!r}->{label!r})"


def main(argv=None):
    ap = argparse.ArgumentParser(description="Viewer <-> LR/ACR 评分双向桥接")
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--dry-run", action="store_true", help="只预览不写入(正向)")
    g.add_argument("--apply", action="store_true", help="正向: 旁车 -> ARW 内嵌")
    g.add_argument("--reverse", action="store_true", help="反向: ARW 内嵌 -> 旁车")
    g.add_argument("--from-list", metavar="FILE",
                   help="正向批量(LR插件用): 每行 path|rating|label, label可空")
    g.add_argument("--reverse-from-list", metavar="FILE",
                   help="反向批量(LR插件用): 每行 path")
    g.add_argument("--launch-viewer", metavar="EXE",
                   help="启动 Viewer(指定 exe 路径); 配合 --folder 打开目录")
    ap.add_argument("--ascii-summary", action="store_true",
                   help="末尾输出ASCII单行摘要(供LR插件解析)")
    ap.add_argument("--folder", metavar="DIR",
                   help="配合 --launch-viewer: 打开后定位到此目录")
    ap.add_argument("targets", nargs="*", help="ARW 文件或文件夹")
    args = ap.parse_args(argv)

    stats = {}

    if args.launch_viewer:
        import subprocess
        cmd = [args.launch_viewer]
        folder = getattr(args, "folder", None)
        if folder:
            cmd.append(folder)
        subprocess.Popen(cmd)
        print("RESULT launched=1")
        return 0

    def run_one(path, fn):
        nonlocal stats
        try:
            status = fn()
        except Exception as e:
            status = f"error({e})"
        key = status.split("(")[0]
        stats[key] = stats.get(key, 0) + 1
        if key not in ("keep", "skip"):
            print(f"  [{status}] {path}")

    if args.from_list:
        # 每行: path|rating|label  (rating 空=不改, label 空=无)
        for line in open(args.from_list, encoding="utf-8-sig"):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split("|")
            p = Path(parts[0])
            r = int(parts[1]) if len(parts) > 1 and parts[1].strip() != "" else None
            lb = parts[2].strip() or None if len(parts) > 2 and parts[2].strip().lower() != "none" else None
            run_one(p, lambda: patch_arw(p, True, r, lb))
    elif args.reverse_from_list:
        for line in open(args.reverse_from_list, encoding="utf-8-sig"):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            p = Path(line)
            run_one(p, lambda: reverse_arw(p))
    else:
        files = []
        for t in args.targets:
            p = Path(t)
            if p.is_dir():
                files += sorted(p.rglob("*.ARW")) + sorted(p.rglob("*.arw"))
            elif p.suffix.lower() == ".arw":
                files.append(p)
        files = sorted(set(files))
        for f in files:
            run_one(f, (lambda ff: lambda: reverse_arw(ff))(f) if args.reverse
                    else (lambda ff: lambda: patch_arw(ff, args.apply))(f))

    print(f"\n共 {sum(stats.values())} 个文件: {stats}")
    if args.ascii_summary:
        # 单行 ASCII 摘要: RESULT patched=N keep=N skip=N created=N updated=N error=N
        keys = ("patched", "keep", "skip", "created", "updated", "error")
        print("RESULT " + " ".join(f"{k}={stats.get(k, 0)}" for k in keys), flush=True)
    if args.dry_run:
        print("(dry-run 预览, 未写入; 加 --apply 执行)")


if __name__ == "__main__":
    main()
