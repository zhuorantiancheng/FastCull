// Read-only LibRaw stage benchmark. Build with the SAME options as the library.
// Usage: bench_raw_pipeline <RAW> <unpack|half|bilinear|ahd> <runs> [threads]
#define LIBRAW_NODLL
#include <libraw/libraw.h>
#include <windows.h>
#include <psapi.h>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#ifdef _OPENMP
#include <omp.h>
#endif
using Clock = std::chrono::steady_clock;
static double ms(Clock::time_point a, Clock::time_point b) {
    return std::chrono::duration<double, std::milli>(b-a).count();
}
static std::string quote(const char *s) {
    std::string v="\"";
    for (; *s; ++s) { if (*s=='"' || *s=='\\') v+='\\'; if ((unsigned char)*s>=32) v+=*s; }
    return v+'"';
}
int wmain(int argc, wchar_t **argv) {
    if (argc<4) return 2;
    std::wstring mode=argv[2]; int runs=_wtoi(argv[3]);
    if (runs<1 || runs>1000 || (mode!=L"unpack" && mode!=L"half" && mode!=L"bilinear" && mode!=L"ahd")) return 2;
    int threads=argc>4?_wtoi(argv[4]):1;
    if (threads<1 || threads>64) return 2;
#ifdef _OPENMP
    omp_set_dynamic(0); omp_set_num_threads(threads);
    const int openmp=1;
#else
    const int openmp=0;
#endif
    printf("{\"libraw\":%s,\"openmp\":%d,\"threads\":%d,\"runs\":[",quote(LibRaw::version()).c_str(),openmp,threads);
    for (int run=0;run<=runs;++run) {
        const auto start=Clock::now();
        LibRaw raw(LIBRAW_OPTIONS_NO_DATAERR_CALLBACK);
        auto &p=raw.imgdata.params;
        p.user_flip=0;p.use_camera_wb=1;p.no_auto_bright=1;p.output_color=1;p.output_bps=8;
        p.user_qual=mode==L"bilinear"?0:3;p.half_size=mode==L"half"?1:0;
        raw.imgdata.rawparams.max_raw_memory_mb=1024;
        int code=raw.open_file(argv[1]);
        if(code){fprintf(stderr,"open: %s\n",libraw_strerror(code));return 1;}
        const auto opened=Clock::now();
        if((code=raw.unpack())){fprintf(stderr,"unpack: %s\n",libraw_strerror(code));return 1;}
        const auto unpacked=Clock::now();
        auto processed=unpacked, exported=unpacked;
        unsigned w=raw.imgdata.sizes.width,h=raw.imgdata.sizes.height;
        unsigned bytes=0; unsigned long long checksum=0;
        const std::string camera=quote(raw.imgdata.idata.model);
        libraw_processed_image_t *image=nullptr;
        if(mode!=L"unpack") {
            if((code=raw.dcraw_process())){fprintf(stderr,"process: %s\n",libraw_strerror(code));return 1;}
            processed=Clock::now(); image=raw.dcraw_make_mem_image(&code);
            if(!image){fprintf(stderr,"export: %s\n",libraw_strerror(code));return 1;}
            exported=Clock::now();w=image->width;h=image->height;bytes=image->data_size;
            for(unsigned i=0;i<bytes;i+=4096) checksum=checksum*131+image->data[i];
        }
        if(image)LibRaw::dcraw_clear_mem(image);
        raw.recycle();const auto finished=Clock::now();
        PROCESS_MEMORY_COUNTERS mem={};mem.cb=sizeof(mem);
        GetProcessMemoryInfo(GetCurrentProcess(),&mem,sizeof(mem));
        printf("%s{\"warmup\":%s,\"open_ms\":%.4f,\"unpack_ms\":%.4f,\"process_ms\":%.4f,\"output_ms\":%.4f,\"total_ms\":%.4f,\"width\":%u,\"height\":%u,\"bytes\":%u,\"checksum\":%llu,\"peak_working_set_bytes\":%llu,\"camera\":%s}",run?",":"",run?"false":"true",ms(start,opened),ms(opened,unpacked),ms(unpacked,processed),ms(processed,exported),ms(start,finished),w,h,bytes,checksum,(unsigned long long)mem.PeakWorkingSetSize,camera.c_str());
        fflush(stdout);
    }
    printf("]}\n");return 0;
}
