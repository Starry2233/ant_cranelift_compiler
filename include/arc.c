#include <stdint.h>
#include <stddef.h>

#if defined(__wasm32__) || defined(__wasm64__)
    #define ARC_WASM_NO_LIBC 1
#else
    #include <stdlib.h>
#endif

#if defined(_MSC_VER)
    #include <windows.h>
    #define ARC_INC(ptr) InterlockedIncrement64((volatile LONGLONG*)(ptr))
    #define ARC_DEC(ptr) InterlockedDecrement64((volatile LONGLONG*)(ptr))
#elif defined(ARC_WASM_NO_LIBC)
    // wasm no-libc path: single-threaded runtime, plain increment/decrement is sufficient.
    #define ARC_INC(ptr) (++(*(size_t*)(ptr)))
    #define ARC_DEC(ptr) (--(*(size_t*)(ptr)))
#else
    #define ARC_INC(ptr) __atomic_add_fetch((size_t*)(ptr), 1, __ATOMIC_RELAXED)
    #define ARC_DEC(ptr) __atomic_sub_fetch((size_t*)(ptr), 1, __ATOMIC_RELAXED)
#endif

// 在堆上分配对象并初始化 ref_count = 1 
void* __obj_alloc(size_t size) { 
#if defined(ARC_WASM_NO_LIBC)
    // Minimal bump allocator for wasm no-libc linking.
    static unsigned char __arc_heap[1024 * 1024 * 4];
    static size_t __arc_off = 0;

    size_t aligned = (size + sizeof(size_t) - 1) & ~(sizeof(size_t) - 1);
    if (__arc_off + aligned > sizeof(__arc_heap)) {
        return NULL;
    }

    size_t* p = (size_t*)(void*)(__arc_heap + __arc_off);
    __arc_off += aligned;
#else
    size_t* p = (size_t*)malloc(size); 
#endif
    if (!p) { 
        return NULL; 
    } 
    *p = 1;  // 第一个字段是 ref_count 
    return (void*)p; 
} 

void __obj_retain(void* p) {
    if (!p) return;
    ARC_INC(p);
}

void __obj_release(void* p) {
    if (!p) return;
    if (ARC_DEC(p) == 0) {
#if !defined(ARC_WASM_NO_LIBC)
        free(p);
#endif
    }
}
