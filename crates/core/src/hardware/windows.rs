//! SetupAPI read-only inventory of currently present display adapters.
use super::Adapter;
use std::{ffi::c_void,io,mem::{size_of,zeroed},ptr::{null,null_mut}};
type Handle=*mut c_void;
#[repr(C)]
struct Guid{a:u32,b:u16,c:u16,d:[u8;8]}
const DISPLAY:Guid=Guid{a:0x4d36e968,b:0xe325,c:0x11ce,d:[0xbf,0xc1,0x08,0x00,0x2b,0xe1,0x03,0x18]};
#[repr(C)]
struct Device{size:u32,class:Guid,devinst:u32,reserved:usize}
#[repr(C)]
struct Key{guid:Guid,pid:u32}
const DRIVER_VERSION:Key=Key{guid:Guid{a:0xa8b865dd,b:0x2e3d,c:0x4094,d:[0xad,0x97,0xe5,0x93,0xa7,0x0c,0x75,0xd6]},pid:3};
#[link(name="setupapi")]
extern "system"{
    fn SetupDiGetClassDevsW(class:*const Guid,enumerator:*const u16,hwnd:Handle,flags:u32)->Handle;
    fn SetupDiEnumDeviceInfo(set:Handle,index:u32,device:*mut Device)->i32;
    fn SetupDiDestroyDeviceInfoList(set:Handle)->i32;
    fn SetupDiGetDeviceRegistryPropertyW(set:Handle,dev:*mut Device,prop:u32,kind:*mut u32,buf:*mut u8,len:u32,required:*mut u32)->i32;
    fn SetupDiGetDeviceInstanceIdW(set:Handle,dev:*mut Device,buf:*mut u16,len:u32,required:*mut u32)->i32;
    fn SetupDiGetDevicePropertyW(set:Handle,dev:*mut Device,key:*const Key,kind:*mut u32,buf:*mut u8,len:u32,required:*mut u32,flags:u32)->i32;
}
struct Set(Handle);
impl Drop for Set{fn drop(&mut self){unsafe{SetupDiDestroyDeviceInfoList(self.0);}}}
fn decode(buf:&[u16])->Option<String>{
    let end=buf.iter().position(|v|*v==0).unwrap_or(buf.len());
    (end>0).then(||String::from_utf16_lossy(&buf[..end]))
}
pub fn adapters()->io::Result<Vec<Adapter>>{
    // SAFETY: owned device set; all output buffers have explicit capacities, initialized
    // UTF-16 alignment, and size fields matching SetupAPI's ABI. No borrowed pointers escape.
    unsafe{
        let h=SetupDiGetClassDevsW(&DISPLAY,null(),null_mut(),2);
        if h==(-1isize as Handle){return Err(io::Error::last_os_error());}
        let set=Set(h);let mut result=Vec::new();
        for i in 0..u32::MAX{
            let mut d:Device=zeroed();d.size=size_of::<Device>() as u32;
            if SetupDiEnumDeviceInfo(set.0,i,&mut d)==0{
                let e=io::Error::last_os_error();
                if e.raw_os_error()==Some(259){break;}else{return Err(e);}
            }
            let mut b=[0u16;4096];let mut kind=0u32;
            let name=if SetupDiGetDeviceRegistryPropertyW(set.0,&mut d,12,&mut kind,b.as_mut_ptr().cast(),8192,null_mut())!=0&&kind==1{decode(&b)}
                else{b.fill(0);if SetupDiGetDeviceRegistryPropertyW(set.0,&mut d,0,&mut kind,b.as_mut_ptr().cast(),8192,null_mut())!=0&&kind==1{decode(&b)}else{None}};
            b.fill(0);
            let instance_id=if SetupDiGetDeviceInstanceIdW(set.0,&mut d,b.as_mut_ptr(),b.len() as u32,null_mut())!=0{decode(&b)}else{None};
            b.fill(0);
            let driver_version=if SetupDiGetDevicePropertyW(set.0,&mut d,&DRIVER_VERSION,&mut kind,b.as_mut_ptr().cast(),8192,null_mut(),0)!=0&&kind==0x12{decode(&b)}else{None};
            result.push(Adapter{name,instance_id,driver_version});
        }
        result.sort_by(|a,b|a.instance_id.cmp(&b.instance_id));
        Ok(result)
    }
}

#[link(name="advapi32")]
extern "system"{
    fn RegGetValueW(key:Handle,subkey:*const u16,value:*const u16,flags:u32,kind:*mut u32,data:*mut c_void,len:*mut u32)->i32;
}
pub fn cpu_name()->Option<String>{
    let sub:Vec<u16>=r"HARDWARE\DESCRIPTION\System\CentralProcessor\0".encode_utf16().chain(Some(0)).collect();
    let value:Vec<u16>="ProcessorNameString".encode_utf16().chain(Some(0)).collect();
    let mut buf=[0u16;256];
    let mut bytes=(buf.len()*2) as u32;
    // SAFETY: HKLM is a documented predefined handle, inputs are NUL terminated,
    // output is initialized, aligned and bounded by bytes.
    let code=unsafe{RegGetValueW(0x80000002u32 as i32 as isize as Handle,sub.as_ptr(),value.as_ptr(),2,null_mut(),buf.as_mut_ptr().cast(),&mut bytes)};
    if code==0{decode(&buf).map(|s|s.trim().to_owned())}else{None}
}
