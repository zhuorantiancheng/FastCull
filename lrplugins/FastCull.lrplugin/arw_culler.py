#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
arw_culler —— 基于 ARW 内嵌全尺寸 JPEG 的高速选片器（LR Classic 风格 UI）

机制：ARW 文件内嵌相机渲染好的全尺寸 JPEG（约 79ms/张即可解码显示），
选片阶段完全不触碰 37MB 的 RAW sensor 数据；打星/颜色/旗标/旋转即时写入
.xmp 旁车，LR（SyncRatings 插件）、ACR、Bridge 均可读取。

UI 与键位对照 Lightroom Classic Loupe/网格视图：
    底部工具栏：旗标(拾取/无/拒绝) | 旋转左右 | 0-5 星 | 5 色标签 | 缩放 | 导航
    底部胶片带：缩略图条，点击跳转，当前照片白框高亮

键位（与 LR 一致）：
    P / X / U           旗标 拾取/拒绝/清除        0-5        星级（0=清星）
    6/7/8/9             颜色 红黄绿蓝（紫=点击）   [ / ]      左/右旋转
    → / 空格 / PgDn     下一张   ← / PgUp 上一张   Home/End   首/末张
    G                   网格视图 ↔ 放大视图 (D/E/回车/双击)
    F / 点击图片        适配窗口 ↔ 100% 像素（方向键/拖动平移）
    Q / Esc             退出（所有改动已即时写盘）

XMP 桥接：xmp:Rating / xmp:Label（标准）；sr:Flag=pick|reject（旗标，LR 目录专用）；
tiff:Orientation（旋转，ACR/Bridge 生效）。

用法：
    python arw_culler.py <目录或若干 .ARW 文件>          # 直接启动
    python arw_culler.py --open <statefile>              # LR 插件调用：
        先把 statefile 里的 LR 评分写入旁车，再分离启动选片器 GUI
    python arw_culler.py --apply-state <statefile>       # 仅写旁车（不启动 GUI）
    python arw_culler.py --from-state <statefile>        # GUI 从 statefile 读路径

statefile 每行: path|rating|label|flag（字段空=无；缺省=不改动；UTF-8）
"""
from __future__ import annotations

import io
import logging
import os
import queue
import re
import struct
import sys
import threading
import time
import traceback
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

try:
    from PIL import Image, ImageOps, ImageTk
    HAS_PIL = True
except ImportError:
    HAS_PIL = False   # headless 子命令(--apply-state/--open 的写旁车)不依赖 PIL

import tkinter as tk


# ----------------------------------------------------------------------------
# 日志：%LOCALAPPDATA%\FastCull\logs\，自动保留最近 20 份，崩溃钩子写完整堆栈
# ----------------------------------------------------------------------------
LOG_DIR = Path(os.path.expandvars(r"%LOCALAPPDATA%\FastCull\logs"))
LOG_KEEP = 20


def setup_logging(interactive=True):
    """interactive=True(GUI) 时未捕获异常弹窗告知日志位置；
    headless(--apply-state/--open) 绝不弹窗——无人值守场景弹窗=挂死进程。"""
    try:
        LOG_DIR.mkdir(parents=True, exist_ok=True)
    except OSError:
        return None
    logfile = LOG_DIR / ("fastcull_app_%s.log"
                         % time.strftime("%Y%m%d_%H%M%S"))
    logging.basicConfig(
        filename=str(logfile), filemode="w", encoding="utf-8",
        level=logging.INFO,
        format="%(asctime)s.%(msecs)03d %(levelname)s %(message)s",
        datefmt="%H:%M:%S")
    # 清理旧日志
    try:
        olds = sorted(LOG_DIR.glob("fastcull_app_*.log"), key=lambda p: p.stat().st_mtime)
        for old in olds[:-LOG_KEEP]:
            old.unlink()
    except OSError:
        pass
    logging.info("FastCull %s python=%s platform=%s pid=%d argv=%r",
                 "1.3", sys.version.split()[0], sys.platform, os.getpid(), sys.argv)

    def _log_exc(tp, val, tb):
        logging.critical("未捕获异常:\n%s",
                         "".join(traceback.format_exception(tp, val, tb)))

    if interactive:
        def _hook(tp, val, tb):
            _log_exc(tp, val, tb)
            try:    # GUI 场景最后弹一次窗告知日志位置
                import tkinter.messagebox as mb
                tmp = tk.Tk()
                tmp.withdraw()
                mb.showerror("FastCull 出错",
                             "发生未处理的错误，程序即将退出。\n\n详细日志:\n%s" % logfile)
                tmp.destroy()
            except Exception:
                pass
        sys.excepthook = _hook
    else:
        # headless：日志 + stdout（Lua 端透出），绝不弹窗，立即退出
        def _hook(tp, val, tb):
            _log_exc(tp, val, tb)
            logging.shutdown()          # 刷新文件缓冲，确保崩溃堆栈落盘
            print("FATAL %s: %s" % (tp.__name__, val))
            print("日志: %s" % logfile)
            os._exit(1)
        sys.excepthook = _hook

    def _thread_hook(args):
        logging.critical("线程 %s 未捕获异常:\n%s", args.thread.name,
                         "".join(traceback.format_exception(args.exc_type,
                                                            args.exc_value,
                                                            args.exc_traceback)))
    threading.excepthook = _thread_hook
    return logfile

# ----------------------------------------------------------------------------
# LR Classic 深色配色
# ----------------------------------------------------------------------------
C_BG      = "#1e1e1e"   # 画布背景
C_PANEL   = "#2b2b2b"   # 工具栏/胶片带/信息条
C_BORDER  = "#3f3f3f"
C_TEXT    = "#c8c8c8"
C_TEXT_HI = "#ececec"
C_STAR_ON = "#e8e8e8"
C_STAR_OFF= "#5a5a5a"
C_SEL     = "#e8e8e8"   # 选中框
C_REJECT  = "#c94f4f"

LR_LABELS = [("Red", "#ce4b42"), ("Yellow", "#e2b937"), ("Green", "#77ab4f"),
             ("Blue", "#5a79a5"), ("Purple", "#a27fa5")]
KEY_LABEL = {6: "Red", 7: "Yellow", 8: "Green", 9: "Blue"}   # LR 键位（紫无键）
DELTA_TO_ORIENTATION = {0: None, 90: "6", 180: "3", 270: "8"}
PREFETCH_AHEAD = 6
IMAGE_CACHE_LIMIT = 4          # 全尺寸图 LRU（约 300MB 峰值）
THUMB_CACHE_LIMIT = 4096       # 缩略图缓存

# ----------------------------------------------------------------------------
# ARW 解析：TIFF IFD → 内嵌 JPEG 位置 + EXIF 摘要
# ----------------------------------------------------------------------------
class ArwInfo:
    __slots__ = ("path", "jpeg_off", "jpeg_len", "orientation", "exif")

    def __init__(self, path: Path):
        self.path = path
        self.jpeg_off = self.jpeg_len = 0
        self.orientation = 1
        self.exif = {}
        self._parse()

    def _parse(self):
        with open(self.path, "rb") as f:
            head = f.read(512 * 1024)   # 所有 IFD/JPEG 指针都在头部区域
        if head[:2] not in (b"II", b"MM"):
            raise ValueError("not TIFF/ARW: %s" % self.path.name)
        e = "<" if head[:2] == b"II" else ">"

        def u16(o): return struct.unpack_from(e + "H", head, o)[0]
        def u32(o): return struct.unpack_from(e + "I", head, o)[0]

        jpegs = []  # (len, off)

        def walk_ifd(off, depth=0, seen=None):
            seen = seen if seen is not None else set()
            if off in seen or off <= 0 or off + 2 > len(head):
                return
            seen.add(off)
            n = u16(off)
            for i in range(n):
                eo = off + 2 + i * 12
                if eo + 12 > len(head):
                    return
                tag, typ, cnt = struct.unpack_from(e + "HHI", head, eo)
                fsize = {1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 7: 1, 9: 4, 10: 8}.get(typ, 1)
                total = fsize * cnt
                vo = eo + 8 if total <= 4 else u32(eo + 8)

                def rational(o):
                    if o + 8 > len(head):
                        return None
                    num, den = struct.unpack_from(e + "II", head, o)
                    return num / den if den else None

                if tag == 0x0112 and typ == 3:              # Orientation
                    self.orientation = struct.unpack_from(e + "H", head, vo)[0]
                elif tag in (0x0201, 0x0202) and typ == 4:  # JPEGInterchangeFormat(/Length)
                    val = u32(vo)
                    if tag == 0x0201:
                        jpegs.append([0, val])
                    elif jpegs:
                        jpegs[-1][0] = val
                elif tag == 0x8769 and typ == 4:            # ExifIFD
                    walk_ifd(u32(vo), depth + 1, seen)
                elif tag == 0x014a and typ == 4:            # SubIFDs (RAW)
                    walk_ifd(u32(vo), depth + 1, seen)
                elif tag == 0x0110 and typ == 2:            # Model
                    self.exif.setdefault("model", self._ascii(head, vo, cnt))
                elif tag == 0x8827 and typ == 3:
                    self.exif["iso"] = struct.unpack_from(e + "H", head, vo)[0]
                elif tag == 0x829d and typ == 5:
                    self.exif["aperture"] = rational(vo)
                elif tag == 0x829a and typ == 5:
                    t = rational(vo)
                    if t:
                        self.exif["shutter"] = t
                elif tag == 0x920a and typ == 5:
                    self.exif["focal"] = rational(vo)

            nxt = u32(off + 2 + n * 12)
            if nxt:
                walk_ifd(nxt, depth, seen)

        walk_ifd(u32(4))

        if not jpegs:
            raise ValueError("no embedded JPEG found: %s" % self.path.name)
        self.jpeg_len, self.jpeg_off = max(jpegs)           # 全尺寸机内 JPEG

    @staticmethod
    def _ascii(buf, off, cnt):
        raw = buf[off:off + cnt]
        return raw.split(b"\0")[0].decode("ascii", "replace").strip()

    def read_jpeg(self) -> bytes:
        with open(self.path, "rb") as f:
            f.seek(self.jpeg_off)
            return f.read(self.jpeg_len)

    def exif_summary(self) -> str:
        parts = []
        if "iso" in self.exif:
            parts.append("ISO%d" % self.exif["iso"])
        if "aperture" in self.exif and self.exif["aperture"]:
            parts.append("f/%.1f" % self.exif["aperture"])
        if "shutter" in self.exif and self.exif["shutter"]:
            t = self.exif["shutter"]
            parts.append("1/%d s" % round(1 / t) if t < 1 else "%.1fs" % t)
        if "focal" in self.exif and self.exif["focal"]:
            parts.append("%gmm" % self.exif["focal"])
        return "  ".join(parts)


# ----------------------------------------------------------------------------
# XMP 旁车读写（原子；已有旁车只替换目标属性，不动其它字段）
# ----------------------------------------------------------------------------
XMP_TEMPLATE = """<?xpacket begin="\xef\xbb\xbf" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="arw_culler">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:tiff="http://ns.adobe.com/tiff/1.0/"
    xmlns:sr="urn:zrtc:syncratings:1/"
{attrs}/>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
"""

# 属性名 -> (写值转换, 是否视为"已设置")
XMP_FIELDS = {
    "rating":  ("xmp:Rating",  lambda v: str(v) if v else "0"),
    "label":   ("xmp:Label",   lambda v: v or ""),
    "flag":    ("sr:Flag",     lambda v: v or ""),
    "orient":  ("tiff:Orientation", lambda v: v or ""),
}
RE_ATTR = re.compile(r'((?:xmp|tiff|sr):(?:Rating|Label|Flag|Orientation))\s*=\s*"([^"]*)"')
RE_ELEM = re.compile(r"<(xmp|tiff|sr):(Rating|Label|Flag|Orientation)>([^<]*)</\1:\2>")


class XmpSidecar:
    def __init__(self, raw_path: Path):
        self.path = raw_path.with_suffix(".xmp")

    # ---------------- 读 ----------------
    def read(self):
        """返回 (rating:int|None, label:str|None, flag:str|None)"""
        try:
            text = self.path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return None, None, None
        vals = {}
        for k, v in RE_ATTR.findall(text):
            vals.setdefault(k.split(":")[1].lower(), v)
        for _ns, k, v in RE_ELEM.findall(text):
            vals.setdefault(k.lower(), v)
        rating = int(vals["rating"]) if vals.get("rating", "").isdigit() else None
        label = vals.get("label") or None
        flag = vals.get("flag") or None
        if flag not in ("pick", "reject"):
            flag = None
        return rating, label, flag

    # ---------------- 写 ----------------
    def write(self, rating=None, label=None, flag=None, orient=None):
        """原子写旁车；None 的字段不动，已有旁车只替换目标属性。

        rating: 0-5（0=清星）；label/flag/orient: ''=清除。
        """
        text = None
        try:
            text = self.path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            pass

        updates = {}
        if rating is not None:
            updates["rating"] = rating
        if label is not None:
            updates["label"] = label
        if flag is not None:
            updates["flag"] = flag
        if orient is not None:
            updates["orient"] = orient
        if not updates:
            return True

        if text is None:
            attrs = []
            for key, val in updates.items():
                name, conv = XMP_FIELDS[key]
                attrs.append('    %s="%s"' % (name, conv(val)))
            out = XMP_TEMPLATE.format(attrs="\n".join(attrs))
        else:
            out = self._patch(text, updates)
            if out is None:
                logging.warning("xmp: %s 已有旁车但无法解析，跳过写入（不破坏原文件）", self.path.name)
                return False

        tmp = self.path.with_suffix(".xmp.tmp")
        tmp.write_text(out, encoding="utf-8", newline="\n")
        os.replace(tmp, self.path)
        return True

    @staticmethod
    def _patch(text, updates):
        out, ok = text, True
        for key, val in updates.items():
            name, conv = XMP_FIELDS[key]
            newattr = '%s="%s"' % (name, conv(val))
            pat = re.compile(r'%s\s*=\s*"[^"]*"' % re.escape(name))
            if pat.search(out):
                out = pat.sub(newattr, out)
                continue
            local = name.split(":")[1]
            elem = re.compile(r"<(?:xmp|tiff|sr):%s>[^<]*</(?:xmp|tiff|sr):%s>"
                              % (local, local))
            if elem.search(out):
                out = elem.sub("<%s>%s</%s>" % (name, conv(val), name), out)
                continue
            # 插入到第一个 rdf:Description 的属性区
            m = re.search(r"<rdf:Description\b[^>]*", out)
            if m:
                close = m.end()
                if out[close - 2:close] == "/>":
                    out = out[:close - 2] + "\n    " + newattr + "/>" + out[close:]
                else:
                    out = out[:close] + "\n    " + newattr + out[close:]
            else:
                ok = False
        return out if ok else None


# ----------------------------------------------------------------------------
# 解码（后台线程返回 PIL Image；PhotoImage 必须在主线程创建）
# ----------------------------------------------------------------------------
def decode_arw(info: ArwInfo):
    im = Image.open(io.BytesIO(info.read_jpeg()))
    im.load()
    im = ImageOps.exif_transpose(im)          # 万一 JPEG 自带 EXIF 方向（防御）
    # Sony 内嵌 JPEG 保持 sensor 横向、无 EXIF 方向，竖拍只由 ARW orientation 表达
    orient = im.getexif().get(274) or info.orientation
    if orient and orient != 1:
        im = _apply_orientation(im, orient)
    return im


def _apply_orientation(im, o):
    return {3: lambda i: i.transpose(Image.ROTATE_180),
            6: lambda i: i.transpose(Image.ROTATE_270),
            8: lambda i: i.transpose(Image.ROTATE_90)}.get(o, lambda i: i)(im)


def _rotate_delta(im, delta):
    return {90: lambda i: i.transpose(Image.ROTATE_270),   # 顺时针 90
            180: lambda i: i.transpose(Image.ROTATE_180),
            270: lambda i: i.transpose(Image.ROTATE_90)
            }.get(delta % 360, lambda i: i)(im)


def make_thumb(im, box):
    t = im.copy()
    t.thumbnail(box, Image.BILINEAR)
    return t


# ----------------------------------------------------------------------------
# 选片器 UI（LR Classic 风格）
# ----------------------------------------------------------------------------
class CullerApp:
    def __init__(self, root: tk.Tk, files):
        self.root = root
        self.files = files
        self.idx = 0
        self.view = "loupe"                   # loupe | grid
        self.infos = {}                       # idx -> ArwInfo|None
        self.states = {}                      # idx -> {rating,label,flag}
        self.rot = {}                         # idx -> delta 0/90/180/270
        self.cache = {}                       # idx -> (full, thumb200, thumb64)
        self.pool = ThreadPoolExecutor(max_workers=1)
        self.pending = set()
        self.msg_q = queue.Queue()            # worker -> UI 线程（tkinter 非线程安全）
        self._pf_futs = []                    # 排队中的预取任务（当前张解码时取消，让其插队）
        self.zoom_100 = False
        self.photo = self.photo_full = None
        self._thumb_imgs = {}                 # PhotoImage 引用防 GC
        self._resize_after = None
        self._grid_after = None

        root.title("ARW 选片器")
        root.configure(bg=C_PANEL)
        root.geometry("+0+0")
        try:
            root.state("zoomed")
        except tk.TclError:
            pass

        self._build_ui()
        self._bind_keys()
        self._poll_ui_queue()                 # 主线程轮询 worker 消息
        self.show()

    # ================= UI 构建 =================
    def _build_ui(self):
        # 顶部信息条
        top = tk.Frame(self.root, bg=C_PANEL)
        top.pack(side="top", fill="x")
        tk.Frame(top, bg=C_BORDER, height=1).pack(side="bottom", fill="x")
        self.lbl_pos = tk.Label(top, fg=C_TEXT, bg=C_PANEL, font=("Segoe UI", 10),
                                anchor="w", padx=12, pady=4)
        self.lbl_pos.pack(side="left")
        self.lbl_exif = tk.Label(top, fg=C_TEXT, bg=C_PANEL, font=("Segoe UI", 10),
                                 anchor="e", padx=12, pady=4)
        self.lbl_exif.pack(side="right")

        # 中部视图区（放大/网格切换）
        self.mid = tk.Frame(self.root, bg=C_BG)
        self.mid.pack(side="top", fill="both", expand=True)

        self.loupe = tk.Canvas(self.mid, bg=C_BG, highlightthickness=0)
        self.loupe.bind("<Button-1>", self._loupe_click)
        self.loupe.bind("<B1-Motion>", lambda e: self.loupe.scan_dragto(e.x, e.y, gain=1))
        self.loupe.pack(fill="both", expand=True)   # 默认放大视图必须布局，否则画布 1x1 不可见

        self.grid_frame = tk.Frame(self.mid, bg=C_BG)
        self.grid = tk.Canvas(self.grid_frame, bg=C_BG, highlightthickness=0)
        self.grid.pack(side="left", fill="both", expand=True)
        gsb = tk.Scrollbar(self.grid_frame, orient="vertical", command=self.grid.yview)
        gsb.pack(side="right", fill="y")
        self.grid.config(yscrollcommand=gsb.set)
        self.grid.bind("<MouseWheel>", self._grid_wheel)
        self.grid.bind("<Button-1>", self._grid_click)
        self.grid.bind("<Double-Button-1>", self._grid_dbl)

        # 底部工具栏（LR：居中）
        bar = tk.Frame(self.root, bg=C_PANEL)
        bar.pack(side="bottom", fill="x")
        tk.Frame(bar, bg=C_BORDER, height=1).pack(side="top", fill="x")
        tb = tk.Frame(bar, bg=C_PANEL)
        tb.pack(anchor="center", pady=3)

        def sep():
            tk.Frame(tb, bg=C_BORDER, width=1).pack(side="left", fill="y", padx=8, pady=2)

        BTN = dict(bd=0, bg=C_PANEL, activebackground="#3d3d3d",
                   relief="flat", takefocus=0, cursor="hand2")

        # -- 旗标组 --
        self.btn_pick = tk.Label(tb, text="\u2691", font=("Segoe UI Symbol", 13),
                                 fg=C_STAR_OFF, **{k: v for k, v in BTN.items() if k != "cursor"})
        self.btn_pick.pack(side="left", padx=3)
        self.btn_pick.bind("<Button-1>", lambda e: self.set_flag("pick"))
        self.btn_unflag = tk.Label(tb, text="\u25cb", font=("Segoe UI Symbol", 12),
                                   fg=C_STAR_OFF, **{k: v for k, v in BTN.items() if k != "cursor"})
        self.btn_unflag.pack(side="left", padx=3)
        self.btn_unflag.bind("<Button-1>", lambda e: self.set_flag(""))
        self.btn_reject = tk.Label(tb, text="\u2297", font=("Segoe UI Symbol", 13),
                                   fg=C_STAR_OFF, **{k: v for k, v in BTN.items() if k != "cursor"})
        self.btn_reject.pack(side="left", padx=3)
        self.btn_reject.bind("<Button-1>", lambda e: self.set_flag("reject"))

        sep()
        # -- 旋转 --
        tk.Label(tb, text="\u21ba", font=("Segoe UI Symbol", 13), fg=C_TEXT,
                 bg=C_PANEL, cursor="hand2").pack(side="left", padx=3)
        self.lbl_rotl = tb.winfo_children()[-1]
        self.lbl_rotl.bind("<Button-1>", lambda e: self.rotate(-90))
        tk.Label(tb, text="\u21bb", font=("Segoe UI Symbol", 13), fg=C_TEXT,
                 bg=C_PANEL, cursor="hand2").pack(side="left", padx=3)
        self.lbl_rotr = tb.winfo_children()[-1]
        self.lbl_rotr.bind("<Button-1>", lambda e: self.rotate(90))

        sep()
        # -- 星级（5 个可点星）--
        self.star_labels = []
        for n in range(1, 6):
            s = tk.Label(tb, text="\u2605", font=("Segoe UI", 13), fg=C_STAR_OFF,
                         bg=C_PANEL, cursor="hand2")
            s.pack(side="left", padx=1)
            s.bind("<Button-1>", lambda e, n=n: self.rate(n))
            self.star_labels.append(s)

        sep()
        # -- 颜色标签（5 色块）--
        self.color_labels = {}
        for name, rgb in LR_LABELS:
            c = tk.Label(tb, width=2, bg=rgb, bd=0, cursor="hand2",
                         highlightthickness=2, highlightbackground=C_PANEL)
            c.pack(side="left", padx=3, pady=3)
            c.bind("<Button-1>", lambda e, n=name: self.set_label(
                "" if self._st()["label"] == n else n))
            self.color_labels[name] = c

        sep()
        # -- 缩放 / 导航 --
        self.lbl_zoom = tk.Label(tb, text="F", font=("Segoe UI", 11, "bold"),
                                 fg=C_TEXT, bg=C_PANEL, width=4, cursor="hand2")
        self.lbl_zoom.pack(side="left", padx=3)
        self.lbl_zoom.bind("<Button-1>", lambda e: self.toggle_zoom())
        self.lbl_prev = tk.Label(tb, text="\u25c0", font=("Segoe UI", 9), fg=C_TEXT,
                                 bg=C_PANEL, cursor="hand2")
        self.lbl_prev.pack(side="left", padx=6)
        self.lbl_prev.bind("<Button-1>", lambda e: self.prev_img())
        self.lbl_next = tk.Label(tb, text="\u25b6", font=("Segoe UI", 9), fg=C_TEXT,
                                 bg=C_PANEL, cursor="hand2")
        self.lbl_next.pack(side="left", padx=6)
        self.lbl_next.bind("<Button-1>", lambda e: self.next_img())

        # 胶片带
        fs = tk.Frame(self.root, bg=C_PANEL)
        fs.pack(side="bottom", fill="x")
        tk.Frame(fs, bg=C_BORDER, height=1).pack(side="top", fill="x")
        self.btn_fs_l = tk.Label(fs, text="\u25c0", fg=C_TEXT, bg=C_PANEL, width=3,
                                 cursor="hand2")
        self.btn_fs_l.pack(side="left", fill="y")
        self.btn_fs_l.bind("<Button-1>", lambda e: self._film_scroll(-1))
        self.film = tk.Canvas(fs, bg=C_PANEL, height=86, highlightthickness=0)
        self.film.pack(side="left", fill="both", expand=True)
        self.film.bind("<Configure>", lambda e: self._draw_film())
        self.btn_fs_r = tk.Label(fs, text="\u25b6", fg=C_TEXT, bg=C_PANEL, width=3,
                                 cursor="hand2")
        self.btn_fs_r.pack(side="left", fill="y")
        self.btn_fs_r.bind("<Button-1>", lambda e: self._film_scroll(1))
        self.film.bind("<Button-1>", self._film_click)

    def _bind_keys(self):
        R = self.root
        for key, fn in [("<Right>", self.next_img), ("<space>", self.next_img),
                        ("<Next>", self.next_img), ("<Left>", self.prev_img),
                        ("<Prior>", self.prev_img),
                        ("<Home>", lambda e: self.goto(0)),
                        ("<End>", lambda e: self.goto(len(self.files) - 1)),
                        ("p", lambda e: self.set_flag("pick")),
                        ("P", lambda e: self.set_flag("pick")),
                        ("x", lambda e: self.set_flag("reject")),
                        ("X", lambda e: self.set_flag("reject")),
                        ("u", lambda e: self.set_flag("")),
                        ("U", lambda e: self.set_flag("")),
                        ("bracketleft", lambda e: self.rotate(-90)),
                        ("bracketright", lambda e: self.rotate(90)),
                        ("g", self._enter_grid), ("G", self._enter_grid),
                        ("d", self._enter_loupe), ("D", self._enter_loupe),
                        ("e", self._enter_loupe), ("E", self._enter_loupe),
                        ("<Return>", self._enter_loupe),
                        ("f", self.toggle_zoom), ("F", self.toggle_zoom),
                        ("l", self._open_logs), ("L", self._open_logs),
                        ("q", self.quit), ("Q", self.quit), ("<Escape>", self.quit)]:
            R.bind(key, fn)
        for n in "0123456789":
            R.bind(n, lambda e, d=n: self.rate(int(d)))
        self.root.bind("<Configure>", self._on_resize)

    # ================= 资源 =================
    def get_info(self, idx):
        if idx not in self.infos:
            try:
                self.infos[idx] = ArwInfo(self.files[idx])
                r, l, f = XmpSidecar(self.files[idx]).read()
                self.states[idx] = {"rating": r, "label": l, "flag": f}
            except Exception as ex:
                logging.warning("arw 解析失败 %s: %s", self.files[idx].name, ex)
                self.infos[idx] = None
                self.states[idx] = {"rating": None, "label": None, "flag": None}
        return self.infos[idx]

    def _st(self, idx=None):
        idx = self.idx if idx is None else idx
        self.get_info(idx)
        return self.states[idx]

    def get_bundle(self, idx):
        if idx in self.cache:
            self.cache[idx] = self.cache.pop(idx)      # LRU touch
            return self.cache[idx]
        info = self.get_info(idx)
        if info is None:
            return None
        # 当前张优先：取消仍在排队的预取任务（正在执行的至多 1 个，~100ms 后让位）
        for f in self._pf_futs:
            f.cancel()
        self._pf_futs = []
        self.pending.clear()      # 被取消任务的 finally 不会执行，pending 一并清空
        t0 = time.time()
        full = self.pool.submit(decode_arw, info).result()
        logging.info("decode idx=%d %.0fms (queue-jump)", idx, (time.time() - t0) * 1000)
        bundle = (full, make_thumb(full, (200, 200)), make_thumb(full, (64, 64)))
        if len(self.cache) >= IMAGE_CACHE_LIMIT:
            self.cache.pop(next(iter(self.cache)))
        self.cache[idx] = bundle
        self._prefetch_ahead()
        return bundle

    def _prefetch_ahead(self):
        for j in range(self.idx + 1, min(self.idx + 1 + PREFETCH_AHEAD, len(self.files))):
            if j in self.cache or j in self.pending or (j in self.infos and not self.infos[j]):
                continue
            self.pending.add(j)
            self._pf_futs.append(self.pool.submit(self._bg_decode, j))

    def _bg_decode(self, j):
        info = self.get_info(j)
        try:
            if info is not None and j not in self.cache:
                full = decode_arw(info)
                bundle = (full, make_thumb(full, (200, 200)), make_thumb(full, (64, 64)))
                if len(self.cache) >= IMAGE_CACHE_LIMIT:
                    self.cache.pop(next(iter(self.cache)))
                self.cache[j] = bundle
                # tkinter 非线程安全：worker 只投消息，UI 刷新全部在主线程
                self.msg_q.put("thumbs")
        except Exception as ex:
            logging.warning("prefetch 失败 %s: %s", self.files[j].name, ex)
        finally:
            self.pending.discard(j)

    def _poll_ui_queue(self):
        """主线程轮询 worker 消息并刷新缩略图区域。"""
        dirty = False
        try:
            while True:
                self.msg_q.get_nowait()
                dirty = True
        except queue.Empty:
            pass
        if dirty:
            if self.view == "grid":
                self._draw_grid()
            self._draw_film()
        try:
            self.root.after(30, self._poll_ui_queue)
        except tk.TclError:
            pass                    # root 已销毁

    # ================= 视图 =================
    def show(self):
        self.zoom_100 = False
        self._render()

    def _enter_grid(self, _e=None):
        self.view = "grid"
        self.loupe.pack_forget()
        self.grid_frame.pack(side="left", fill="both", expand=True)
        self._draw_grid()

    def _enter_loupe(self, _e=None):
        self.view = "loupe"
        self.grid_frame.pack_forget()
        self.loupe.pack(fill="both", expand=True)
        self.show()

    def _render(self):
        if self.view == "grid":
            self._draw_grid()
        else:
            self._draw_loupe()
        self._draw_film()
        self._update_toolbar()

    def _display_image(self, idx):
        """全尺寸图 + 用户旋转 delta。"""
        b = self.get_bundle(idx)
        if b is None:
            return None
        d = self.rot.get(idx, 0)
        return _rotate_delta(b[0], d) if d else b[0]

    def _draw_loupe(self):
        im = self._display_image(self.idx)
        if im is None:
            self.loupe.delete("all")
            self.loupe.create_text(
                self.loupe.winfo_width() // 2 or 400,
                self.loupe.winfo_height() // 2 or 300,
                text="无法解码：%s" % self.files[self.idx].name, fill=C_REJECT)
            self._update_infobar()
            return
        try:
            if self.zoom_100:
                self.photo_full = ImageTk.PhotoImage(im)
                self.loupe.delete("all")
                self.loupe.create_image(0, 0, anchor="nw", image=self.photo_full)
                self.loupe.config(scrollregion=(0, 0, im.width, im.height))
            else:
                vw = max(self.loupe.winfo_width(), 200)
                vh = max(self.loupe.winfo_height(), 200)
                fit = im.copy()
                fit.thumbnail((vw, vh), Image.BILINEAR)
                photo = ImageTk.PhotoImage(fit)   # 先建后清：失败保留旧画面
                self.photo = photo
                self.loupe.delete("all")
                x = max((vw - fit.width) // 2, 0)
                y = max((vh - fit.height) // 2, 0)
                self.loupe.create_image(x, y, anchor="nw", image=photo)
                self.loupe.config(scrollregion=(0, 0, vw, vh))
            logging.info("loupe drawn idx=%d canvas=%dx%d items=%d zoom100=%s",
                         self.idx, self.loupe.winfo_width(), self.loupe.winfo_height(),
                         len(self.loupe.find_all()), self.zoom_100)
        except Exception:
            logging.exception("主图渲染失败 idx=%d", self.idx)
            raise
        self._update_infobar()

    # ---- 网格视图 ----
    CELL_W, CELL_H, COLS = 180, 224, 6

    def _draw_grid(self):
        cv = self.grid
        cv.delete("all")
        self._thumb_imgs.clear()
        n = len(self.files)
        cols = max(1, min(self.COLS, cv.winfo_width() // self.CELL_W or self.COLS))
        rows = (n + cols - 1) // cols
        cv.config(scrollregion=(0, 0, cols * self.CELL_W, rows * self.CELL_H))
        parsed = 0
        for i in range(n):
            r, c = divmod(i, cols)
            x0, y0 = c * self.CELL_W, r * self.CELL_H
            cx = x0 + self.CELL_W // 2
            # 缩略图
            if i in self.states or parsed < 8:     # 每帧最多同步解析 8 张
                self.get_info(i)
                parsed += 1
            thumb = None
            if i in self.cache:
                t = self.cache[i][1]
                d = self.rot.get(i, 0)
                if d:
                    t = _rotate_delta(t, d)
                thumb = t
            ph = None
            if thumb is not None:
                ph = ImageTk.PhotoImage(thumb)
                self._thumb_imgs[i] = ph
            if ph:
                cv.create_image(cx, y0 + 8, anchor="n", image=ph)
            else:
                cv.create_rectangle(cx - 40, y0 + 8, cx + 40, y0 + 128,
                                    fill="#262626", outline=C_BORDER)
            # 元数据行
            st = self.states.get(i, {})
            stars = "".join("\u2605" if k < (st.get("rating") or 0) else "\u2606"
                            for k in range(5))
            flag = {"pick": "\u2691", "reject": "\u2297"}.get(st.get("flag"), "")
            cv.create_text(x0 + 6, y0 + self.CELL_H - 44, anchor="w",
                           text=flag, fill=C_STAR_ON if flag == "\u2691" else C_REJECT,
                           font=("Segoe UI Symbol", 10))
            cv.create_text(x0 + 24, y0 + self.CELL_H - 46, anchor="w",
                           text=stars, fill=C_STAR_ON if st.get("rating") else C_STAR_OFF,
                           font=("Segoe UI", 9))
            lab = st.get("label")
            if lab:
                rgb = dict(LR_LABELS).get(lab, "#888")
                cv.create_rectangle(x0 + self.CELL_W - 16, y0 + self.CELL_H - 50,
                                    x0 + self.CELL_W - 6, y0 + self.CELL_H - 40,
                                    fill=rgb, outline="")
            cv.create_text(x0 + 6, y0 + self.CELL_H - 24, anchor="w",
                           text=self.files[i].name[:24], fill=C_TEXT,
                           font=("Segoe UI", 8))
            # 当前照片白框
            if i == self.idx:
                cv.create_rectangle(x0 + 3, y0 + 4, x0 + self.CELL_W - 3,
                                    y0 + self.CELL_H - 4, outline=C_SEL, width=2)
        self._update_infobar()

    def _grid_pos(self, ev):
        cv = self.grid
        cols = max(1, min(self.COLS, cv.winfo_width() // self.CELL_W or self.COLS))
        x = cv.canvasx(ev.x)
        y = cv.canvasy(ev.y)
        c, r = int(x // self.CELL_W), int(y // self.CELL_H)
        i = r * cols + c
        return i if 0 <= i < len(self.files) else None

    def _grid_click(self, ev):
        i = self._grid_pos(ev)
        if i is not None:
            self.idx = i
            self._draw_grid()
            self._draw_film()
            self._update_toolbar()
            self._update_infobar()

    def _grid_dbl(self, ev):
        i = self._grid_pos(ev)
        if i is not None:
            self.idx = i
            self._enter_loupe()

    def _grid_wheel(self, ev):
        self.grid.yview_scroll(-1 * (ev.delta // 120), "units")

    # ---- 胶片带 ----
    FS_W = 78

    def _draw_film(self):
        cv = self.film
        cv.delete("all")
        w = max(cv.winfo_width(), 100)
        n = len(self.files)
        visible = w // self.FS_W + 2
        start = max(0, min(self.idx - visible // 2, max(n - visible, 0)))
        self._film_start = start
        parsed = 0
        for i in range(start, min(start + visible + 2, n)):
            x0 = (i - start) * self.FS_W
            cx = x0 + self.FS_W // 2
            ph = None
            if i in self.cache:
                t = self.cache[i][2]
                d = self.rot.get(i, 0)
                if d:
                    t = _rotate_delta(t, d)
                ph = ImageTk.PhotoImage(t)
                self._thumb_imgs[("f", i)] = ph
            if ph:
                cv.create_image(cx, 43, image=ph)
            elif parsed < 2:                 # 每帧最多同步解析 2 张（防大文件夹卡顿）
                self.get_info(i)
                parsed += 1
                cv.create_rectangle(cx - 30, 11, cx + 30, 71, fill="#232323",
                                    outline=C_BORDER)
            st = self.states.get(i, {})
            if st.get("flag") == "reject":
                cv.create_line(x0 + 8, 8, x0 + self.FS_W - 8, 74, fill=C_REJECT, width=2)
                cv.create_line(x0 + 8, 74, x0 + self.FS_W - 8, 8, fill=C_REJECT, width=2)
            if i == self.idx:
                cv.create_rectangle(x0 + 2, 3, x0 + self.FS_W - 2, 79,
                                    outline=C_SEL, width=2)

    def _film_click(self, ev):
        i = int(self.film.canvasx(ev.x) // self.FS_W)
        i += self._film_start or 0
        if 0 <= i < len(self.files):
            self.idx = i
            if self.view == "grid":
                self._draw_grid()
            else:
                self.show()
            self._draw_film()

    _film_start = 0

    def _film_scroll(self, _d):
        self.idx = max(0, min(self.idx + 4 * _d, len(self.files) - 1))
        self.goto(self.idx)

    # ================= 操作 =================
    def next_img(self, _e=None):
        if self.idx < len(self.files) - 1:
            self.idx += 1
            self.show()

    def prev_img(self, _e=None):
        if self.idx > 0:
            self.idx -= 1
            self.show()

    def goto(self, idx):
        self.idx = max(0, min(idx, len(self.files) - 1))
        self.show()

    def _commit(self, **kw):
        ok = XmpSidecar(self.files[self.idx]).write(**kw)
        if ok:
            detail = " ".join("%s=%s" % (k, v if v not in ("", None) else "-")
                              for k, v in kw.items())
            logging.info("xmp %s %s", self.files[self.idx].name, detail)
        self._update_toolbar()
        if self.view == "grid":
            self._draw_grid()

    def rate(self, d):
        if d in KEY_LABEL:
            self.set_label("" if self._st()["label"] == KEY_LABEL[d] else KEY_LABEL[d])
            return
        self._st()["rating"] = d
        self._commit(rating=d)

    def set_label(self, name):
        self._st()["label"] = name or None
        self._commit(label=name or "")

    def set_flag(self, flag):
        self._st()["flag"] = flag or None
        self._commit(flag=flag or "")

    def rotate(self, d):
        nd = (self.rot.get(self.idx, 0) + d) % 360
        self.rot[self.idx] = nd
        self._commit(orient=DELTA_TO_ORIENTATION[nd] or "")
        if self.view == "loupe":
            self.show()
        else:
            self._draw_grid()
        self._draw_film()

    def toggle_zoom(self, _e=None):
        self.zoom_100 = not self.zoom_100
        self._draw_loupe()

    def _loupe_click(self, ev):
        # 拖动结束与单击区分：位移小视为点击切换缩放（LR 行为）
        x0 = [ev.x, ev.y]

        def on_release(e):
            if abs(e.x - x0[0]) + abs(e.y - x0[1]) < 6:
                self.toggle_zoom()
            self.loupe.unbind("<ButtonRelease-1>")
        self.loupe.bind("<ButtonRelease-1>", on_release)

    # ================= 状态显示 =================
    def _update_infobar(self):
        f = self.files[self.idx]
        info = self.infos.get(self.idx)
        self.lbl_pos.config(text="%d / %d    %s" % (self.idx + 1, len(self.files), f.name))
        self.lbl_exif.config(text=info.exif_summary() if info else "")

    def _update_toolbar(self):
        st = self._st()
        r = st.get("rating") or 0
        for n, s in enumerate(self.star_labels, 1):
            s.config(fg=C_STAR_ON if n <= r else C_STAR_OFF)
        flag = st.get("flag")
        self.btn_pick.config(fg=C_STAR_ON if flag == "pick" else C_STAR_OFF)
        self.btn_reject.config(fg=C_REJECT if flag == "reject" else C_STAR_OFF)
        self.btn_unflag.config(fg=C_TEXT if not flag else C_STAR_OFF)
        cur = st.get("label")
        for name, c in self.color_labels.items():
            c.config(highlightbackground=C_SEL if name == cur else C_PANEL)
        self.lbl_zoom.config(text="1:1" if self.zoom_100 else "F")

    def _on_resize(self, _e):
        if self._resize_after:
            self.root.after_cancel(self._resize_after)
        self._resize_after = self.root.after(200, self._render)

    def _open_logs(self, _e=None):
        try:
            os.startfile(str(LOG_DIR))
        except Exception as ex:
            logging.warning("打开日志目录失败: %s", ex)

    def quit(self, _e=None):
        logging.info("退出")
        self.pool.shutdown(wait=False)
        self.root.destroy()


# ----------------------------------------------------------------------------
# 状态文件（LR 插件 -> 选片器）：每行 path|rating|label|flag
# ----------------------------------------------------------------------------
def parse_state_file(path):
    """rating/label/flag 空串=LR 无该值（写清），字段缺失=不改动。"""
    items = []
    for line in open(path, encoding="utf-8-sig"):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split("|")
        p = parts[0]
        r = parts[1] if len(parts) > 1 else None
        l = parts[2] if len(parts) > 2 else None
        fg = parts[3] if len(parts) > 3 else None
        # LR 的 pick: 1=pick, -1=reject, 0=无
        if fg is not None:
            fg = {"1": "pick", "-1": "reject"}.get(fg, "") or ""
        items.append((Path(p), int(r) if r and r.isdigit() else None, l, fg))
    return items


def apply_state(items):
    """把 LR 目录中的评分/颜色/旗标落到旁车（读取方向）。"""
    stats = {"wrote": 0, "kept": 0, "error": 0}
    for p, rating, label, flag in items:
        try:
            sc = XmpSidecar(p)
            if not sc.path.exists() and not rating and not label and not flag:
                stats["kept"] += 1
                continue
            if sc.write(rating=rating, label=label, flag=flag):
                stats["wrote"] += 1
            else:
                stats["error"] += 1
        except Exception as ex:
            logging.warning("apply-state 失败 %s: %s", p.name, ex)
            stats["error"] += 1
    return stats


def _launch_gui_detached(statefile):
    """脱离当前进程启动选片器 GUI（供 LR 的 io.popen 链调用）。"""
    import subprocess
    subprocess.Popen(
        [sys.executable, os.path.abspath(__file__), "--from-state", statefile],
        creationflags=subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP,
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, close_fds=True)


def cmd_open(statefile):
    # 管道下 Windows 默认 cp936，重配为 UTF-8 容错，防中文文件名导致 print 崩溃
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    items = parse_state_file(statefile)
    stats = apply_state(items)
    print("RESULT wrote=%d kept=%d error=%d" % (
        stats["wrote"], stats["kept"], stats["error"]))
    logging.info("open: apply wrote=%d kept=%d error=%d",
                 stats["wrote"], stats["kept"], stats["error"])
    if not HAS_PIL:
        print("RESULT launched=0 (Pillow 未安装：请运行 %s -m pip install pillow)"
              % Path(sys.executable).name)
        return 1
    try:
        _launch_gui_detached(statefile)
        print("RESULT launched=1")
        logging.info("open: GUI 已分离启动")
    except Exception as ex:
        print("RESULT launched=0 (%s)" % ex)
        return 1
    return 0


# ----------------------------------------------------------------------------
def collect_files(args):
    out = []
    for a in args:
        p = Path(a)
        if p.is_dir():
            out += sorted(q for q in p.iterdir() if q.suffix.lower() == ".arw")
        elif p.suffix.lower() == ".arw":
            out.append(p)
    return out


def main():
    argv = sys.argv[1:]
    headless = bool(argv) and argv[0] in ("--apply-state", "--open")
    setup_logging(interactive=not headless)
    if not argv or argv[0] in ("-h", "--help"):
        print(__doc__)
        return
    if argv[0] == "--apply-state" and len(argv) > 1:
        stats = apply_state(parse_state_file(argv[1]))
        print("RESULT wrote=%d kept=%d error=%d" % (
            stats["wrote"], stats["kept"], stats["error"]))
        logging.info("apply-state: wrote=%d kept=%d error=%d",
                     stats["wrote"], stats["kept"], stats["error"])
        return
    if argv[0] == "--open" and len(argv) > 1:
        sys.exit(cmd_open(argv[1]))
    if argv[0] == "--from-state" and len(argv) > 1:
        files = [p for p, _, _, _ in parse_state_file(argv[1])]
    else:
        files = collect_files(argv)
    if not files:
        logging.error("没有找到 .ARW 文件")
        sys.exit("没有找到 .ARW 文件")
    if not HAS_PIL:
        logging.error("缺少 Pillow")
        sys.exit("需要 Pillow（显示内嵌 JPEG）：请运行 python -m pip install pillow")
    logging.info("GUI 启动，共 %d 张", len(files))
    print("共 %d 张" % len(files))
    root = tk.Tk()
    CullerApp(root, files)
    root.mainloop()


if __name__ == "__main__":
    main()
