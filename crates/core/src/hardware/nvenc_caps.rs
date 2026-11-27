//! Windows x64 read-only NVENC capability query, using the public SDK 12.1 ABI.
//! ABI source: FFmpeg/nv-codec-headers n12.1.14.0, include/ffnvcodec/nvEncodeAPI.h.
//! No encoder initialization, input surfaces, frame submission or external process.
//! Unknown or incomplete GPU evidence is not interpreted as support.
//!
//! The following ABI declarations are derived from NVIDIA's MIT-licensed header:
//! Copyright (c) 2010-2023 NVIDIA Corporation
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
//! of the Software, and to permit persons to whom the Software is furnished to do
//! so, subject to the following conditions:
//! The above copyright notice and this permission notice shall be included in
//! all copies or substantial portions of the Software.
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
//! THE SOFTWARE.
use super::NvencCaps;
use crate::budget::Codec;
use std::{ffi::{c_void,c_char,CStr},mem::{size_of,zeroed},ptr::null_mut};
type Handle=*mut c_void;
#[repr(C)]
#[derive(Clone,Copy,PartialEq,Eq)]
struct Guid{a:u32,b:u16,c:u16,d:[u8;8]}
const H264:Guid=Guid{a:0x6bc82762,b:0x4e63,c:0x4ca4,d:[0xaa,0x85,0x1e,0x50,0xf3,0x21,0xf6,0xbf]};
const HEVC:Guid=Guid{a:0x790cdc88,b:0x4522,c:0x4d7b,d:[0x94,0x25,0xbd,0xa9,0x97,0x5f,0x76,0x03]};
const AV1:Guid=Guid{a:0x0a352289,b:0x0aa7,c:0x4759,d:[0x86,0x2d,0x5d,0x15,0xcd,0x16,0xd2,0x54]};
const API:u32=12|(1<<24);
const fn version(v:u32)->u32 {API|(v<<16)|(7<<28)}
#[repr(C)]
struct Functions{version:u32,reserved:u32,slots:[Handle;318]}
#[repr(C)]
struct Open{version:u32,device_type:u32,device:Handle,reserved:Handle,api_version:u32,reserved1:[u32;253],reserved2:[Handle;64]}
#[repr(C)]
struct Caps{version:u32,query:u32,reserved:[u32;62]}
const _:()=assert!(size_of::<Guid>()==16 && size_of::<Functions>()==2552 && size_of::<Open>()==1552 && size_of::<Caps>()==256);
#[link(name="kernel32")]
extern "system"{
    fn LoadLibraryExW(name:*const u16,file:Handle,flags:u32)->Handle;
    fn GetProcAddress(module:Handle,name:*const c_char)->Handle;
    fn FreeLibrary(module:Handle)->i32;
}
struct Library(Handle);
impl Library {
    fn load(name:&str)->Option<Self>{
        let name:Vec<u16>=name.encode_utf16().chain(Some(0)).collect();
        // Only System32: never load DLLs from the portable or working directory.
        let handle=unsafe{LoadLibraryExW(name.as_ptr(),null_mut(),0x800)};
        if handle.is_null(){None}else{Some(Self(handle))}
    }
    fn symbol(&self,name:&CStr)->Option<Handle>{
        let p=unsafe{GetProcAddress(self.0,name.as_ptr())};
        (!p.is_null()).then_some(p)
    }
}
impl Drop for Library{fn drop(&mut self){unsafe{FreeLibrary(self.0);}}}
type Init=unsafe extern "system" fn(u32)->i32;
type Count=unsafe extern "system" fn(*mut i32)->i32;
type Device=unsafe extern "system" fn(*mut i32,i32)->i32;
type Retain=unsafe extern "system" fn(*mut Handle,i32)->i32;
type Release=unsafe extern "system" fn(i32)->i32;
type Create=unsafe extern "system" fn(*mut Functions)->i32;
type OpenSession=unsafe extern "system" fn(*mut Open,*mut Handle)->i32;
type Destroy=unsafe extern "system" fn(Handle)->i32;
type Guids=unsafe extern "system" fn(Handle,*mut Guid,u32,*mut u32)->i32;
type GetCaps=unsafe extern "system" fn(Handle,Guid,*mut Caps,*mut i32)->i32;
struct Context{device:i32,release:Release}
impl Drop for Context{fn drop(&mut self){unsafe{(self.release)(self.device);}}}
struct Session{handle:Handle,destroy:Destroy}
impl Drop for Session{fn drop(&mut self){unsafe{(self.destroy)(self.handle);}}}
impl Session{
    fn cap(&self,get:GetCaps,guid:Guid,query:u32)->Option<i32>{
        let mut p=Caps{version:version(1),query,reserved:[0;62]};
        let mut value=0;
        // Session is live, the ABI is pinned, all reserved inputs zeroed.
        (unsafe{get(self.handle,guid,&mut p,&mut value)}==0).then_some(value)
    }
}
pub(super) fn query(codec:Codec)->Option<NvencCaps>{
    let guid=match codec{Codec::H264=>H264,Codec::Hevc=>HEVC,Codec::Av1=>AV1};
    let cuda=Library::load("nvcuda.dll")?;
    let nv=Library::load("nvEncodeAPI64.dll")?;
    // SAFETY: Windows x64 ABI and SDK 12.1 layout verified above. Driver-owned
    // symbols and function-table entries are checked non-null; exact documented
    // signatures are used. RAII releases sessions before their CUDA contexts and
    // libraries. All buffers are initialized and exclusively owned by this thread.
    unsafe{
        let init:Init=std::mem::transmute(cuda.symbol(c"cuInit")?);
        let count:Count=std::mem::transmute(cuda.symbol(c"cuDeviceGetCount")?);
        let device:Device=std::mem::transmute(cuda.symbol(c"cuDeviceGet")?);
        let retain:Retain=std::mem::transmute(cuda.symbol(c"cuDevicePrimaryCtxRetain")?);
        let release:Release=std::mem::transmute(cuda.symbol(c"cuDevicePrimaryCtxRelease_v2").or_else(||cuda.symbol(c"cuDevicePrimaryCtxRelease"))?);
        let create:Create=std::mem::transmute(nv.symbol(c"NvEncodeAPICreateInstance")?);
        let mut table:Functions=zeroed();table.version=version(2);
        if create(&mut table)!=0 || init(0)!=0{return None;}
        if [4,7,27,29].iter().any(|&i|table.slots[i].is_null()){return None;}
        let guids:Guids=std::mem::transmute(table.slots[4]);
        let get:GetCaps=std::mem::transmute(table.slots[7]);
        let destroy:Destroy=std::mem::transmute(table.slots[27]);
        let open:OpenSession=std::mem::transmute(table.slots[29]);
        let mut n=0;
        if count(&mut n)!=0 || !(1..=64).contains(&n){return None;}
        let mut result:Option<NvencCaps>=None;
        for index in 0..n{
            let mut dev=0;
            if device(&mut dev,index)!=0{return None;}
            let mut ctx=null_mut();
            if retain(&mut ctx,dev)!=0{return None;}
            let _context=Context{device:dev,release};
            if ctx.is_null(){return None;}
            let mut params:Open=zeroed();params.version=version(1);params.device_type=1;
            params.device=ctx;params.api_version=API;
            let mut handle=null_mut();
            if open(&mut params,&mut handle)!=0 || handle.is_null(){return None;}
            let session=Session{handle,destroy};
            let mut ids:[Guid;64]=zeroed();let mut len=0;
            if guids(handle,ids.as_mut_ptr(),ids.len() as u32,&mut len)!=0 || len as usize>ids.len(){return None;}
            if !ids[..len as usize].contains(&guid){continue;}
            let caps=NvencCaps{
                max_b_frames:u32::try_from(session.cap(get,guid,0)?).ok()?,
                lookahead:session.cap(get,guid,37)?>0,
                temporal_aq:session.cap(get,guid,38)?>0,
                ten_bit:session.cap(get,guid,39)?>0,
                middle_ref:session.cap(get,guid,43)?&2!=0,
            };
            result=Some(result.map_or(caps,|old|old.intersection(caps)));
        }
        result
    }
}
