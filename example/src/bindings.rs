// Generated by `wit-bindgen` 0.19.1. DO NOT EDIT!
// Options used:
pub mod example {
    pub mod component {

        #[allow(clippy::all)]
        pub mod cache {
            #[used]
            #[doc(hidden)]
            #[cfg(target_arch = "wasm32")]
            static __FORCE_SECTION_REF: fn() = super::super::super::__link_section;
            #[allow(unused_unsafe, clippy::all)]
            /// Get a value from the cache.
            pub fn get(key: &str) -> Option<wit_bindgen::rt::vec::Vec<u8>> {
                #[allow(unused_imports)]
                use wit_bindgen::rt::{alloc, string::String, vec::Vec};
                unsafe {
                    #[repr(align(4))]
                    struct RetArea([u8; 12]);
                    let mut ret_area = ::core::mem::MaybeUninit::<RetArea>::uninit();
                    let vec0 = key;
                    let ptr0 = vec0.as_ptr() as i32;
                    let len0 = vec0.len() as i32;
                    let ptr1 = ret_area.as_mut_ptr() as i32;
                    #[cfg(target_arch = "wasm32")]
                    #[link(wasm_import_module = "example:component/cache")]
                    extern "C" {
                        #[link_name = "get"]
                        fn wit_import(_: i32, _: i32, _: i32);
                    }

                    #[cfg(not(target_arch = "wasm32"))]
                    fn wit_import(_: i32, _: i32, _: i32) {
                        unreachable!()
                    }
                    wit_import(ptr0, len0, ptr1);
                    let l2 = i32::from(*((ptr1 + 0) as *const u8));
                    match l2 {
                        0 => None,
                        1 => {
                            let e = {
                                let l3 = *((ptr1 + 4) as *const i32);
                                let l4 = *((ptr1 + 8) as *const i32);
                                let len5 = l4 as usize;

                                Vec::from_raw_parts(l3 as *mut _, len5, len5)
                            };
                            Some(e)
                        }
                        _ => wit_bindgen::rt::invalid_enum_discriminant(),
                    }
                }
            }
            #[allow(unused_unsafe, clippy::all)]
            /// Put a value into the cache.
            pub fn put(key: &str, value: &[u8]) {
                #[allow(unused_imports)]
                use wit_bindgen::rt::{alloc, string::String, vec::Vec};
                unsafe {
                    let vec0 = key;
                    let ptr0 = vec0.as_ptr() as i32;
                    let len0 = vec0.len() as i32;
                    let vec1 = value;
                    let ptr1 = vec1.as_ptr() as i32;
                    let len1 = vec1.len() as i32;

                    #[cfg(target_arch = "wasm32")]
                    #[link(wasm_import_module = "example:component/cache")]
                    extern "C" {
                        #[link_name = "put"]
                        fn wit_import(_: i32, _: i32, _: i32, _: i32);
                    }

                    #[cfg(not(target_arch = "wasm32"))]
                    fn wit_import(_: i32, _: i32, _: i32, _: i32) {
                        unreachable!()
                    }
                    wit_import(ptr0, len0, ptr1, len1);
                }
            }
        }

        #[allow(clippy::all)]
        pub mod backend {
            #[used]
            #[doc(hidden)]
            #[cfg(target_arch = "wasm32")]
            static __FORCE_SECTION_REF: fn() = super::super::super::__link_section;
            #[allow(unused_unsafe, clippy::all)]
            /// Fetch the content bytes of the given URL.
            pub fn fetch(url: &str) -> wit_bindgen::rt::vec::Vec<u8> {
                #[allow(unused_imports)]
                use wit_bindgen::rt::{alloc, string::String, vec::Vec};
                unsafe {
                    #[repr(align(4))]
                    struct RetArea([u8; 8]);
                    let mut ret_area = ::core::mem::MaybeUninit::<RetArea>::uninit();
                    let vec0 = url;
                    let ptr0 = vec0.as_ptr() as i32;
                    let len0 = vec0.len() as i32;
                    let ptr1 = ret_area.as_mut_ptr() as i32;
                    #[cfg(target_arch = "wasm32")]
                    #[link(wasm_import_module = "example:component/backend")]
                    extern "C" {
                        #[link_name = "fetch"]
                        fn wit_import(_: i32, _: i32, _: i32);
                    }

                    #[cfg(not(target_arch = "wasm32"))]
                    fn wit_import(_: i32, _: i32, _: i32) {
                        unreachable!()
                    }
                    wit_import(ptr0, len0, ptr1);
                    let l2 = *((ptr1 + 0) as *const i32);
                    let l3 = *((ptr1 + 4) as *const i32);
                    let len4 = l3 as usize;
                    Vec::from_raw_parts(l2 as *mut _, len4, len4)
                }
            }
        }
    }
}
pub mod exports {
    pub mod example {
        pub mod component {

            #[allow(clippy::all)]
            pub mod backend {
                #[used]
                #[doc(hidden)]
                #[cfg(target_arch = "wasm32")]
                static __FORCE_SECTION_REF: fn() = super::super::super::super::__link_section;
                const _: () = {
                    #[doc(hidden)]
                    #[export_name = "example:component/backend#fetch"]
                    #[allow(non_snake_case)]
                    unsafe extern "C" fn __export_fetch(arg0: i32, arg1: i32) -> i32 {
                        #[allow(unused_imports)]
                        use wit_bindgen::rt::{alloc, string::String, vec::Vec};

                        // Before executing any other code, use this function to run all static
                        // constructors, if they have not yet been run. This is a hack required
                        // to work around wasi-libc ctors calling import functions to initialize
                        // the environment.
                        //
                        // This functionality will be removed once rust 1.69.0 is stable, at which
                        // point wasi-libc will no longer have this behavior.
                        //
                        // See
                        // https://github.com/bytecodealliance/preview2-prototyping/issues/99
                        // for more details.
                        #[cfg(target_arch = "wasm32")]
                        wit_bindgen::rt::run_ctors_once();

                        let len0 = arg1 as usize;
                        let bytes0 = Vec::from_raw_parts(arg0 as *mut _, len0, len0);
                        let result1 =
                            <_GuestImpl as Guest>::fetch(wit_bindgen::rt::string_lift(bytes0));
                        let ptr2 = _RET_AREA.0.as_mut_ptr() as i32;
                        let vec3 = (result1).into_boxed_slice();
                        let ptr3 = vec3.as_ptr() as i32;
                        let len3 = vec3.len() as i32;
                        ::core::mem::forget(vec3);
                        *((ptr2 + 4) as *mut i32) = len3;
                        *((ptr2 + 0) as *mut i32) = ptr3;
                        ptr2
                    }

                    const _: () = {
                        #[doc(hidden)]
                        #[export_name = "cabi_post_example:component/backend#fetch"]
                        #[allow(non_snake_case)]
                        unsafe extern "C" fn __post_return_fetch(arg0: i32) {
                            let l0 = *((arg0 + 0) as *const i32);
                            let l1 = *((arg0 + 4) as *const i32);
                            let base2 = l0;
                            let len2 = l1;
                            wit_bindgen::rt::dealloc(base2, (len2 as usize) * 1, 1);
                        }
                    };
                };
                use super::super::super::super::super::Component as _GuestImpl;
                pub trait Guest {
                    /// Fetch the content bytes of the given URL.
                    fn fetch(url: wit_bindgen::rt::string::String)
                        -> wit_bindgen::rt::vec::Vec<u8>;
                }

                #[allow(unused_imports)]
                use wit_bindgen::rt::{alloc, string::String, vec::Vec};

                #[repr(align(4))]
                struct _RetArea([u8; 8]);
                static mut _RET_AREA: _RetArea = _RetArea([0; 8]);
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[link_section = "component-type:example"]
#[doc(hidden)]
pub static __WIT_BINDGEN_COMPONENT_TYPE: [u8; 354] = [
    0, 97, 115, 109, 13, 0, 1, 0, 0, 25, 22, 119, 105, 116, 45, 99, 111, 109, 112, 111, 110, 101,
    110, 116, 45, 101, 110, 99, 111, 100, 105, 110, 103, 4, 0, 7, 228, 1, 1, 65, 2, 1, 65, 6, 1,
    66, 6, 1, 112, 125, 1, 107, 0, 1, 64, 1, 3, 107, 101, 121, 115, 0, 1, 4, 0, 3, 103, 101, 116,
    1, 2, 1, 64, 2, 3, 107, 101, 121, 115, 5, 118, 97, 108, 117, 101, 0, 1, 0, 4, 0, 3, 112, 117,
    116, 1, 3, 3, 1, 23, 101, 120, 97, 109, 112, 108, 101, 58, 99, 111, 109, 112, 111, 110, 101,
    110, 116, 47, 99, 97, 99, 104, 101, 5, 0, 1, 66, 3, 1, 112, 125, 1, 64, 1, 3, 117, 114, 108,
    115, 0, 0, 4, 0, 5, 102, 101, 116, 99, 104, 1, 1, 3, 1, 25, 101, 120, 97, 109, 112, 108, 101,
    58, 99, 111, 109, 112, 111, 110, 101, 110, 116, 47, 98, 97, 99, 107, 101, 110, 100, 5, 1, 1,
    66, 3, 1, 112, 125, 1, 64, 1, 3, 117, 114, 108, 115, 0, 0, 4, 0, 5, 102, 101, 116, 99, 104, 1,
    1, 4, 1, 25, 101, 120, 97, 109, 112, 108, 101, 58, 99, 111, 109, 112, 111, 110, 101, 110, 116,
    47, 98, 97, 99, 107, 101, 110, 100, 5, 2, 4, 1, 25, 101, 120, 97, 109, 112, 108, 101, 58, 99,
    111, 109, 112, 111, 110, 101, 110, 116, 47, 101, 120, 97, 109, 112, 108, 101, 4, 0, 11, 13, 1,
    0, 7, 101, 120, 97, 109, 112, 108, 101, 3, 0, 0, 0, 71, 9, 112, 114, 111, 100, 117, 99, 101,
    114, 115, 1, 12, 112, 114, 111, 99, 101, 115, 115, 101, 100, 45, 98, 121, 2, 13, 119, 105, 116,
    45, 99, 111, 109, 112, 111, 110, 101, 110, 116, 7, 48, 46, 50, 48, 48, 46, 48, 16, 119, 105,
    116, 45, 98, 105, 110, 100, 103, 101, 110, 45, 114, 117, 115, 116, 6, 48, 46, 49, 57, 46, 49,
];

#[inline(never)]
#[doc(hidden)]
#[cfg(target_arch = "wasm32")]
pub fn __link_section() {
    wit_bindgen::rt::maybe_link_cabi_realloc();
}
