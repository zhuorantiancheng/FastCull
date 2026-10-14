// Read-only, hot-cache input-path comparison. Link the same LibRaw build for all modes.
// Usage: bench_raw_input RAW file|buffer|mmap unpack|half RUNS THREADS
#define LIBRAW_NODLL
#include <libraw/libraw.h>
#include <windows.h>
#include <omp.h>
#include <chrono>
#include <cstdio>
#include <string>
#include <vector>
#include <stdexcept>
using Clock = std::chrono::steady_clock;
static double elapsed(Clock::time_point a, Clock::time_point b) {
    return std::chrono::duration<double, std::milli>(b-a).count();
}
struct Input {
    HANDLE file=INVALID_HANDLE_VALUE, mapping=nullptr;
    void *view=nullptr;
    std::vector<unsigned char> bytes;
    ~Input() {
        if(view) UnmapViewOfFile(view);
        if(mapping) CloseHandle(mapping);
        if(file!=INVALID_HANDLE_VALUE) CloseHandle(file);
    }
    int open(LibRaw &raw, const wchar_t *path, const std::wstring &kind) {
        if(kind==L"file") return raw.open_file(path);
        file=CreateFileW(path,GENERIC_READ,FILE_SHARE_READ,nullptr,OPEN_EXISTING,FILE_ATTRIBUTE_NORMAL,nullptr);
        LARGE_INTEGER size={};
        if(file==INVALID_HANDLE_VALUE || !GetFileSizeEx(file,&size) || size.QuadPart<=0 || size.QuadPart>0x7fffffff)
            throw std::runtime_error("input open/size failed");
        if(kind==L"mmap") {
            mapping=CreateFileMappingW(file,nullptr,PAGE_READONLY,0,0,nullptr);
            if(!mapping) throw std::runtime_error("file mapping failed");
            view=MapViewOfFile(mapping,FILE_MAP_READ,0,0,0);
            if(!view) throw std::runtime_error("mapping view failed");
            return raw.open_buffer(view,static_cast<size_t>(size.QuadPart));
        }
        bytes.resize(static_cast<size_t>(size.QuadPart));
        DWORD count=0;
        if(!ReadFile(file,bytes.data(),static_cast<DWORD>(bytes.size()),&count,nullptr) || count!=bytes.size())
            throw std::runtime_error("input read failed");
        return raw.open_buffer(bytes.data(),bytes.size());
    }
};
int wmain(int argc,wchar_t **argv) {
    if(argc!=6) return 2;
    const std::wstring kind=argv[2],mode=argv[3];
    int runs=_wtoi(argv[4]),threads=_wtoi(argv[5]);
    if((kind!=L"file" && kind!=L"buffer" && kind!=L"mmap") || (mode!=L"unpack" && mode!=L"half") || runs<1 || runs>100 || threads<1 || threads>64) return 2;
    omp_set_dynamic(0); omp_set_num_threads(threads);
    printf("{\"libraw\":\"%s\",\"threads\":%d,\"runs\":[",LibRaw::version(),threads);
    try {
        for(int run=0;run<=runs;++run) {
            auto start=Clock::now();
            Input input; // Outlives LibRaw and its buffer datastream.
            LibRaw raw(LIBRAW_OPTIONS_NO_DATAERR_CALLBACK);
            auto &p=raw.imgdata.params;
            p.user_flip=0;p.use_camera_wb=1;p.no_auto_bright=1;p.output_color=1;p.output_bps=8;
            p.user_qual=3;p.half_size=mode==L"half"?1:0;
            raw.imgdata.rawparams.max_raw_memory_mb=1024;
            int code=input.open(raw,argv[1],kind);
            if(code) throw std::runtime_error(libraw_strerror(code));
            auto opened=Clock::now();
            std::string decoder=raw.unpack_function_name();
            if((code=raw.unpack())) throw std::runtime_error(libraw_strerror(code));
            auto unpacked=Clock::now(),processed=unpacked,exported=unpacked;
            libraw_processed_image_t *image=nullptr;
            if(mode==L"half") {
                if((code=raw.dcraw_process())) throw std::runtime_error(libraw_strerror(code));
                processed=Clock::now();image=raw.dcraw_make_mem_image(&code);
                if(!image) throw std::runtime_error(libraw_strerror(code));
                exported=Clock::now();
            }
            // Full output hash, outside measured timings. Input modes must agree.
            unsigned long long hash=14695981039346656037ULL;
            const unsigned char *data=image?image->data:reinterpret_cast<unsigned char*>(raw.imgdata.rawdata.raw_image);
            size_t count=image?image->data_size:size_t(raw.imgdata.sizes.raw_width)*raw.imgdata.sizes.raw_height*2;
            if(!data) throw std::runtime_error("expected Bayer raw_image");
            for(size_t i=0;i<count;++i) { hash^=data[i];hash*=1099511628211ULL; }
            printf("%s{\"warmup\":%s,\"open_ms\":%.4f,\"unpack_ms\":%.4f,\"process_ms\":%.4f,\"output_ms\":%.4f,\"ready_ms\":%.4f,\"checksum\":\"%llu\",\"decoder\":\"%s\",\"warnings\":%u}",
                run?",":"",run?"false":"true",elapsed(start,opened),elapsed(opened,unpacked),elapsed(unpacked,processed),elapsed(processed,exported),elapsed(start,exported),hash,decoder.c_str(),raw.imgdata.process_warnings);
            if(image) LibRaw::dcraw_clear_mem(image);
            fflush(stdout);
        }
    } catch(const std::exception &e) { fprintf(stderr,"%s\n",e.what());return 1; }
    printf("]}\n");return 0;
}
