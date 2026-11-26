//! Read-only system snapshot; FFI is confined to this module.
use serde_json::{json,Value};
use std::{ffi::c_void,mem::size_of,ptr::{null_mut}};

#[repr(C)]
struct Version { size:u32,major:u32,minor:u32,build:u32,platform:u32,service_pack:[u16;128] }
#[repr(C)]
struct Memory { size:u32,load:u32,total_phys:u64,avail_phys:u64,total_page:u64,avail_page:u64,total_virtual:u64,avail_virtual:u64,avail_extended:u64 }
#[repr(C)]
struct Power { ac:u8,battery_flag:u8,battery_percent:u8,system_status:u8,battery_life:u32,full_life:u32 }
#[repr(C)]
struct Guid { a:u32,b:u16,c:u16,d:[u8;8] }
#[link(name="ntdll")]
extern "system" { fn RtlGetVersion(v:*mut Version)->i32; }
#[link(name="kernel32")]
extern "system" {
    fn GetSystemTimes(idle:*mut FileTime,kernel:*mut FileTime,user:*mut FileTime)->i32;
    fn GlobalMemoryStatusEx(v:*mut Memory)->i32;
    fn GetSystemPowerStatus(v:*mut Power)->i32;
    fn LocalFree(p:*mut c_void)->*mut c_void;
}
#[link(name="powrprof")]
extern "system" { fn PowerGetActiveScheme(root:*mut c_void,guid:*mut *mut Guid)->u32; }

pub(super) fn snapshot()->Value {
    let mut v=Version{size:size_of::<Version>() as u32,major:0,minor:0,build:0,platform:0,service_pack:[0;128]};
    let mut m=Memory{size:size_of::<Memory>() as u32,load:0,total_phys:0,avail_phys:0,total_page:0,avail_page:0,total_virtual:0,avail_virtual:0,avail_extended:0};
    let mut p=Power{ac:255,battery_flag:255,battery_percent:255,system_status:0,battery_life:0,full_life:0};
    let mut g:*mut Guid=null_mut();
    // SAFETY: ABI-compatible initialized structs with correct sizes and exclusive
    // writable pointers. PowerGetActiveScheme allocates a GUID with LocalAlloc;
    // read only on success and release the returned allocation with LocalFree.
    unsafe {
        let version=if RtlGetVersion(&mut v)==0 {json!({"major":v.major,"minor":v.minor,"build":v.build})}else{Value::Null};
        let memory=if GlobalMemoryStatusEx(&mut m)!=0 {json!({"total_physical_bytes":m.total_phys,"available_physical_bytes":m.avail_phys,"load_percent":m.load})}else{Value::Null};
        let power=if GetSystemPowerStatus(&mut p)!=0 {json!({"ac_line":match p.ac{0=>"battery",1=>"ac",_=>"unknown"},"battery_saver":p.system_status==1})}else{Value::Null};
        let status=PowerGetActiveScheme(null_mut(),&mut g);
        let scheme=if status==0&&!g.is_null(){let x=&*g;Some(format!("{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",x.a,x.b,x.c,x.d[0],x.d[1],x.d[2],x.d[3],x.d[4],x.d[5],x.d[6],x.d[7]))}else{None};
        if !g.is_null(){let _=LocalFree(g.cast());}
        json!({"version":version,"memory":memory,"power":power,"power_scheme_guid":scheme})
    }
}

#[repr(C)]
#[derive(Clone,Copy,Default)]
struct FileTime{low:u32,high:u32}
impl FileTime{fn ticks(self)->u64{(u64::from(self.high)<<32)|u64::from(self.low)}}
fn cpu_times()->Option<(u64,u64,u64)>{
    let(mut a,mut b,mut c)=(FileTime::default(),FileTime::default(),FileTime::default());
    // SAFETY: three distinct initialized writable FILETIME buffers.
    if unsafe{GetSystemTimes(&mut a,&mut b,&mut c)}==0{return None}
    Some((a.ticks(),b.ticks(),c.ticks()))
}
pub(super) fn pretest_cpu_busy()->Option<f64>{
    let a=cpu_times()?;std::thread::sleep(std::time::Duration::from_millis(250));let b=cpu_times()?;
    let idle=b.0.checked_sub(a.0)?;let total=b.1.checked_sub(a.1)?.checked_add(b.2.checked_sub(a.2)?)?;
    if total==0||idle>total{return None}
    Some(100.0*(total-idle) as f64/total as f64)
}
