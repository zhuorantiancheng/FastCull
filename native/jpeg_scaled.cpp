// Windows' JPEG decoder can skip inverse-DCT work at 1/2, 1/4 and 1/8 size.
// Used only for reduced previews; full-resolution decoding stays in Rust.
#define NOMINMAX
#include <windows.h>
#include <wincodec.h>
#include <wrl/client.h>
#include <algorithm>
#include <cstdint>
#include <cstdlib>
#include <memory>
using Microsoft::WRL::ComPtr;
struct FCFrame {
    uint32_t kind, width, height, channels, bits, rendered;
    uint64_t len;
    const unsigned char *data;
    void *owner;
};
struct Apartment {
    HRESULT status = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    ~Apartment() { if (SUCCEEDED(status)) CoUninitialize(); }
};
#define CHECK_WIC(expr) do { HRESULT hr=(expr); if(FAILED(hr)) return int(hr); } while(0)
extern "C" __declspec(dllexport) int fc_jpeg_scaled(
    const unsigned char *data, uint32_t length, uint32_t width, uint32_t height, FCFrame *out) {
    *out = {};
    try {
        Apartment apartment;
        if (FAILED(apartment.status) && apartment.status != RPC_E_CHANGED_MODE) return int(apartment.status);
        ComPtr<IWICImagingFactory> factory;
        CHECK_WIC(CoCreateInstance(CLSID_WICImagingFactory, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&factory)));
        ComPtr<IWICStream> stream;
        CHECK_WIC(factory->CreateStream(&stream));
        CHECK_WIC(stream->InitializeFromMemory(const_cast<BYTE*>(data), length));
        ComPtr<IWICBitmapDecoder> decoder;
        // Pin the built-in JPEG codec; do not probe third-party camera codecs.
        CHECK_WIC(factory->CreateDecoder(GUID_ContainerFormatJpeg, &GUID_VendorMicrosoft, &decoder));
        CHECK_WIC(decoder->Initialize(stream.Get(), WICDecodeMetadataCacheOnDemand));
        ComPtr<IWICBitmapFrameDecode> frame;
        CHECK_WIC(decoder->GetFrame(0, &frame));
        ComPtr<IWICBitmapSourceTransform> transform;
        CHECK_WIC(frame.As(&transform));
        UINT w=width,h=height;
        CHECK_WIC(transform->GetClosestSize(&w,&h));
        if(w<width || h<height || !w || !h || uint64_t(w)*h>160000000) return int(E_INVALIDARG);
        WICPixelFormatGUID format=GUID_WICPixelFormat24bppBGR;
        CHECK_WIC(transform->GetClosestPixelFormat(&format));
        if(!IsEqualGUID(format,GUID_WICPixelFormat24bppBGR)) return int(E_NOTIMPL);
        const size_t count=size_t(w)*h;
        std::unique_ptr<unsigned char, decltype(&free)> bgr(static_cast<unsigned char*>(malloc(count*3)), &free);
        std::unique_ptr<unsigned char, decltype(&free)> rgba(static_cast<unsigned char*>(malloc(count*4)), &free);
        if(!bgr || !rgba) return int(E_OUTOFMEMORY);
        CHECK_WIC(transform->CopyPixels(nullptr,w,h,&format,WICBitmapTransformRotate0,w*3,UINT(count*3),bgr.get()));
        for(size_t i=0;i<count;++i) {
            rgba.get()[4*i]=bgr.get()[3*i+2];
            rgba.get()[4*i+1]=bgr.get()[3*i+1];
            rgba.get()[4*i+2]=bgr.get()[3*i];
            rgba.get()[4*i+3]=255;
        }
        out->kind=2;out->width=w;out->height=h;out->channels=4;out->bits=8;
        out->len=count*4;out->data=rgba.get();out->owner=rgba.release();
        return 0;
    } catch(...) { return int(E_FAIL); }
}
extern "C" __declspec(dllexport) void fc_jpeg_release(void *pixels) { free(pixels); }
