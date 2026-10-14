// FastCull C ABI over LibRaw. Each call owns a decoder; no shared camera state.
#define LIBRAW_NODLL
#include <libraw/libraw.h>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <ctime>
#include <exception>

#ifdef _WIN32
#define FC_EXPORT extern "C" __declspec(dllexport)
using FCPathChar = wchar_t;
#else
#define FC_EXPORT extern "C" __attribute__((visibility("default")))
using FCPathChar = char; // UTF-8 on macOS, never wchar_t (32 bits on Unix).
#endif

struct FCInfo {
    uint32_t abi, width, height, orientation, preview_width, preview_height;
    float iso, aperture, shutter, focal;
    char make[64], model[64], lens[128], captured[32];
};
struct FCFrame {
    uint32_t kind, width, height, channels, bits, rendered;
    uint64_t len;
    const unsigned char *data;
    void *owner;
};
static int fail(char *error, const char *text) {
    snprintf(error, 256, "%s", text ? text : "LibRaw error");
    return -1;
}
static bool dimensions(unsigned w, unsigned h) {
    return w && h && uint64_t(w)*h <= 160000000;
}
static unsigned orientation(int flip) {
    static const unsigned map[] = {1,2,4,3,5,8,6,7};
    return flip >= 0 && flip < 8 ? map[flip] : 1;
}
struct ExifSize {unsigned width=0,height=0;};
static void size_callback(void *context,int tag,int type,int count,unsigned order,void *input,INT64) {
    if ((tag!=0xa002 && tag!=0xa003) || count!=1 || (type!=3 && type!=4)) return;
    auto *stream=static_cast<LibRaw_abstract_datastream*>(input);
    const auto saved=stream->tell();unsigned char bytes[4]={};
    const unsigned length=type==3?2:4;
    int read=stream->read(bytes,1,length);stream->seek(saved,SEEK_SET);
    if (read!=int(length)) return;
    unsigned value=0;
    if (order==0x4949) for(int i=int(length)-1;i>=0;--i) value=(value<<8)|bytes[i];
    else if(order==0x4d4d) for(unsigned i=0;i<length;++i) value=(value<<8)|bytes[i];
    else return;
    auto *size=static_cast<ExifSize*>(context);
    if(tag==0xa002)size->width=value;else size->height=value;
}
static int open_camera(LibRaw &raw,const FCPathChar *path) {
    ExifSize exif;
    raw.set_exifparser_handler(size_callback,&exif);
    int code=raw.open_file(path);
    raw.set_exifparser_handler(nullptr,nullptr);
    if(code)return code;
    // Camera output excludes small sensor border regions. Use an explicit
    // vendor crop when available, then the recorded EXIF output dimensions.
    raw.adjust_to_raw_inset_crop(1,0.95f);
    auto &s=raw.imgdata.sizes;
    // Older NEFs omit an output-area tag. A near-full preview can differ only
    // by the sensor border: at most 64 pixels and 1% on each axis. This never
    // treats a reduced-resolution thumbnail as the original-size image.
    const auto &thumb=raw.imgdata.thumbnail;
    if (!dimensions(exif.width,exif.height) && raw.imgdata.idata.maker_index==LIBRAW_CAMERAMAKER_Nikon &&
        dimensions(thumb.twidth,thumb.theight) && thumb.twidth<=s.width && thumb.theight<=s.height &&
        s.width-thumb.twidth<=64 && s.height-thumb.theight<=64 &&
        uint64_t(thumb.twidth)*100>=uint64_t(s.width)*99 && uint64_t(thumb.theight)*100>=uint64_t(s.height)*99) {
        exif.width=thumb.twidth;exif.height=thumb.theight;
    }
    if(dimensions(exif.width,exif.height) && exif.width<=s.width && exif.height<=s.height &&
       uint64_t(exif.width)*100>=uint64_t(s.width)*95 && uint64_t(exif.height)*100>=uint64_t(s.height)*95) {
        s.left_margin+=((s.width-exif.width)/2)&~1u;
        s.top_margin+=((s.height-exif.height)/2)&~1u;
        s.width=exif.width;s.height=exif.height;
        auto &backup=raw.imgdata.rawdata.sizes;
        backup.left_margin=s.left_margin;backup.top_margin=s.top_margin;
        backup.width=s.width;backup.height=s.height;
    }
    return 0;
}
static bool jpeg_size(const unsigned char *p, size_t n, unsigned &w, unsigned &h) {
    if (n < 4 || p[0]!=255 || p[1]!=216) return false;
    size_t i=2;
    while (i+4<=n) {
        if (p[i++]!=255) return false;
        while (i<n && p[i]==255) ++i;
        if (i>=n) return false;
        unsigned marker=p[i++];
        if (marker==218 || marker==217) return false;
        if (marker==1 || (marker>=208 && marker<=215)) continue;
        if (i+2>n) return false;
        unsigned len=(p[i]<<8)|p[i+1];
        if (len<2 || len>n-i) return false;
        if (marker>=192 && marker<=207 && marker!=196 && marker!=200 && marker!=204) {
            if (len<8) return false;
            h=(p[i+3]<<8)|p[i+4]; w=(p[i+5]<<8)|p[i+6];
            return dimensions(w,h);
        }
        i+=len;
    }
    return false;
}
FC_EXPORT int fc_raw_info(const FCPathChar *path, FCInfo *out, char *error) {
    try {
        LibRaw raw(LIBRAW_OPTIONS_NO_DATAERR_CALLBACK);
        raw.imgdata.rawparams.max_raw_memory_mb=1024;
        int code=open_camera(raw,path);
        if (code) return fail(error,libraw_strerror(code));
        const auto &d=raw.imgdata;
        if (!dimensions(d.sizes.width,d.sizes.height)) return fail(error,"RAW dimensions exceed 160 MP limit");
        memset(out,0,sizeof(*out)); out->abi=1;
        out->width=d.sizes.width; out->height=d.sizes.height;
        out->orientation=orientation(d.sizes.flip);
        out->preview_width=d.thumbnail.twidth;out->preview_height=d.thumbnail.theight;
        out->iso=d.other.iso_speed;out->aperture=d.other.aperture;
        out->shutter=d.other.shutter;out->focal=d.other.focal_len;
        snprintf(out->make,sizeof(out->make),"%s",d.idata.make);
        snprintf(out->model,sizeof(out->model),"%s",d.idata.model);
        snprintf(out->lens,sizeof(out->lens),"%s",d.lens.Lens);
        if (d.other.timestamp>0) {
            tm time={};
#ifdef _WIN32
            const bool valid_time = !localtime_s(&time,&d.other.timestamp);
#else
            const bool valid_time = localtime_r(&d.other.timestamp,&time) != nullptr;
#endif
            if (valid_time) strftime(out->captured,sizeof(out->captured),"%Y:%m:%d %H:%M:%S",&time);
        }
        return 0;
    } catch(const std::exception &e) { return fail(error,e.what()); }
      catch(...) { return fail(error,"LibRaw metadata exception"); }
}
FC_EXPORT int fc_raw_decode(const FCPathChar *path, int full, FCFrame *out, char *error) {
    try {
        memset(out,0,sizeof(*out));
        LibRaw raw(LIBRAW_OPTIONS_NO_DATAERR_CALLBACK);
        raw.imgdata.rawparams.max_raw_memory_mb=1024;
        auto &p=raw.imgdata.params;
        p.user_flip=0; p.use_camera_wb=1;p.no_auto_bright=1;p.output_color=1;p.output_bps=8;
        p.user_qual=3;p.half_size=full?0:1;
        int code=open_camera(raw,path);
        if (code) return fail(error,libraw_strerror(code));
        if (!dimensions(raw.imgdata.sizes.width,raw.imgdata.sizes.height)) return fail(error,"RAW dimensions exceed 160 MP limit");
        libraw_processed_image_t *image=nullptr;
        if (!raw.unpack_thumb()) image=raw.dcraw_make_mem_thumb(&code);
        unsigned w=image?image->width:0,h=image?image->height:0;
        if (image && image->type==LIBRAW_IMAGE_JPEG && !jpeg_size(image->data,image->data_size,w,h)) {
            LibRaw::dcraw_clear_mem(image);image=nullptr;w=h=0;
        }
        // Byte length is not a reliable proxy for resolution. Check additional
        // previews whose dimensions are larger or absent from the metadata.
        for (int i=1;i<raw.imgdata.thumbs_list.thumbcount;++i) {
            const auto &candidate=raw.imgdata.thumbs_list.thumblist[i];
            if (candidate.twidth && candidate.theight && uint64_t(candidate.twidth)*candidate.theight<=uint64_t(w)*h) continue;
            if (raw.unpack_thumb_ex(i)) continue;
            auto *next=raw.dcraw_make_mem_thumb(&code);
            if (!next) continue;
            unsigned nw=next->width,nh=next->height;
            bool valid=next->type==LIBRAW_IMAGE_JPEG?jpeg_size(next->data,next->data_size,nw,nh):dimensions(nw,nh);
            if (valid && uint64_t(nw)*nh>uint64_t(w)*h) {
                if (image) LibRaw::dcraw_clear_mem(image);
                image=next;w=nw;h=nh;
            } else LibRaw::dcraw_clear_mem(next);
        }
        if (image && ((image->type!=LIBRAW_IMAGE_JPEG && image->type!=LIBRAW_IMAGE_BITMAP) ||
            (image->type==LIBRAW_IMAGE_BITMAP && (image->bits!=8 || image->colors>4)))) {
            LibRaw::dcraw_clear_mem(image);image=nullptr;
        }
        bool rendered=false;
        if (!image || (full && (w+2<raw.imgdata.sizes.width || h+2<raw.imgdata.sizes.height))) {
            if (image) LibRaw::dcraw_clear_mem(image);
            image=nullptr;
            if ((code=raw.unpack()) || (code=raw.dcraw_process())) return fail(error,libraw_strerror(code));
            image=raw.dcraw_make_mem_image(&code);rendered=true;
            if (!image) return fail(error,libraw_strerror(code));
            w=image->width;h=image->height;
        }
        if (!dimensions(w,h) || (image->type!=LIBRAW_IMAGE_JPEG && image->type!=LIBRAW_IMAGE_BITMAP)) {
            LibRaw::dcraw_clear_mem(image);return fail(error,"Unsupported preview encoding or dimensions");
        }
        out->kind=image->type==LIBRAW_IMAGE_JPEG?1:2;
        out->width=w;out->height=h;out->channels=image->colors;out->bits=image->bits;
        out->rendered=rendered?1:0;out->len=image->data_size;out->data=image->data;out->owner=image;
        return 0;
    } catch(const std::exception &e) { return fail(error,e.what()); }
      catch(...) { return fail(error,"LibRaw decode exception"); }
}
FC_EXPORT void fc_raw_release(void *image) {
    if (image) LibRaw::dcraw_clear_mem(static_cast<libraw_processed_image_t*>(image));
}
