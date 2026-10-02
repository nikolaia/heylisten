//! System audio on macOS 14.4+: a Core Audio process tap of everything except this process,
//! read through a private aggregate device. No virtual drivers needed.

use std::ffi::{CStr, c_void};
use std::mem::{MaybeUninit, size_of};
use std::ptr::{NonNull, null_mut};

use anyhow::{Result, bail};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_audio::*;
use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, kAudioFormatFlagIsFloat};
use objc2_core_foundation::{CFDictionary, CFRetained, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUUID};

use super::{Sink, downmix};

pub struct System {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    ctx: *mut Ctx,
    pub device: String,
}

struct Ctx {
    sink: Sink,
    rate: u32,
    mono: Vec<f32>,
    scratch: Vec<f32>,
}

impl System {
    pub fn start(sink: Sink) -> Result<System> {
        // Cleanup on early return is handled by Drop.
        let mut sys = System { tap: 0, aggregate: 0, proc_id: None, ctx: null_mut(), device: String::new() };
        unsafe {
            let me: AudioObjectID = get(kAudioObjectSystemObject as _, kAudioHardwarePropertyTranslatePIDToProcessObject, Some(std::process::id() as i32))?;
            let excluded = if me == 0 { NSArray::new() } else { NSArray::from_retained_slice(&[NSNumber::new_u32(me)]) };
            let desc = CATapDescription::initStereoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &excluded);
            desc.setPrivate(true);
            desc.setMuteBehavior(CATapMuteBehavior::Unmuted);
            check(AudioHardwareCreateProcessTap(Some(&desc), &mut sys.tap), "create the system audio tap")?;

            let format: AudioStreamBasicDescription = get(sys.tap, kAudioTapPropertyFormat, None)?;
            if format.mFormatFlags & kAudioFormatFlagIsFloat == 0 || format.mBitsPerChannel != 32 {
                bail!("unexpected system audio format: {format:?}");
            }

            let output: AudioObjectID = get(kAudioObjectSystemObject as _, kAudioHardwarePropertyDefaultOutputDevice, None)?;
            let output_uid: *const CFString = get(output, kAudioDevicePropertyDeviceUID, None)?;
            let output_uid = CFRetained::from_raw(NonNull::new(output_uid as *mut CFString).expect("no output device UID")).to_string();
            let name: *const CFString = get(output, kAudioObjectPropertyName, None)?;
            sys.device = NonNull::new(name as *mut CFString)
                .map_or_else(|| output_uid.clone(), |n| CFRetained::from_raw(n).to_string());

            let output_uid = NSString::from_str(&output_uid);
            let sub_device = dict(&[(kAudioSubDeviceUIDKey, &*output_uid)]);
            let sub_tap = dict(&[
                (kAudioSubTapUIDKey, &*desc.UUID().UUIDString()),
                (kAudioSubTapDriftCompensationKey, &*NSNumber::new_bool(true)),
            ]);
            let description = dict(&[
                (kAudioAggregateDeviceNameKey, &*NSString::from_str("heyListen system audio")),
                (kAudioAggregateDeviceUIDKey, &*NSUUID::new().UUIDString()),
                (kAudioAggregateDeviceMainSubDeviceKey, &*output_uid),
                (kAudioAggregateDeviceIsPrivateKey, &*NSNumber::new_bool(true)),
                (kAudioAggregateDeviceIsStackedKey, &*NSNumber::new_bool(false)),
                (kAudioAggregateDeviceTapAutoStartKey, &*NSNumber::new_bool(true)),
                (kAudioAggregateDeviceSubDeviceListKey, &*NSArray::from_retained_slice(&[sub_device])),
                (kAudioAggregateDeviceTapListKey, &*NSArray::from_retained_slice(&[sub_tap])),
            ]);
            let description = &*(Retained::as_ptr(&description) as *const CFDictionary); // toll-free bridged
            check(
                AudioHardwareCreateAggregateDevice(description, NonNull::from(&mut sys.aggregate)),
                "create the aggregate device for system audio",
            )?;

            let rate = format.mSampleRate as u32;
            sys.ctx = Box::into_raw(Box::new(Ctx { sink, rate, mono: Vec::new(), scratch: Vec::new() }));
            check(
                AudioDeviceCreateIOProcID(sys.aggregate, Some(io_proc), sys.ctx as *mut c_void, NonNull::from(&mut sys.proc_id)),
                "register the system audio callback",
            )?;
            check(AudioDeviceStart(sys.aggregate, sys.proc_id), "start system audio capture")?;
        }
        Ok(sys)
    }
}

impl Drop for System {
    fn drop(&mut self) {
        unsafe {
            if self.proc_id.is_some() {
                AudioDeviceStop(self.aggregate, self.proc_id);
                AudioDeviceDestroyIOProcID(self.aggregate, self.proc_id);
            }
            if self.aggregate != 0 {
                AudioHardwareDestroyAggregateDevice(self.aggregate);
            }
            if self.tap != 0 {
                AudioHardwareDestroyProcessTap(self.tap);
            }
            if !self.ctx.is_null() {
                drop(Box::from_raw(self.ctx));
            }
        }
    }
}

unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let ctx = unsafe { &mut *(client as *mut Ctx) };
    let list = unsafe { input.as_ref() };
    let buffers = unsafe { std::slice::from_raw_parts(list.mBuffers.as_ptr(), list.mNumberBuffers as usize) };
    ctx.mono.clear();
    for (i, b) in buffers.iter().enumerate() {
        if b.mData.is_null() {
            continue;
        }
        let samples = unsafe { std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4) };
        downmix(samples, b.mNumberChannels as usize, &mut ctx.scratch);
        if i == 0 {
            ctx.mono.extend_from_slice(&ctx.scratch);
        } else {
            ctx.mono.iter_mut().zip(&ctx.scratch).for_each(|(m, s)| *m += s);
        }
    }
    if buffers.len() > 1 {
        let n = buffers.len() as f32;
        ctx.mono.iter_mut().for_each(|m| *m /= n);
    }
    let time = unsafe { input_time.as_ref() };
    let captured = if time.mFlags.contains(objc2_core_audio_types::AudioTimeStampFlags::HostTimeValid) {
        super::host_ticks_to_ns(time.mHostTime)
    } else {
        super::now_ns()
    };
    (ctx.sink)(&ctx.mono, ctx.rate, captured);
    0
}

fn dict(pairs: &[(&CStr, &AnyObject)]) -> Retained<NSDictionary<NSString, AnyObject>> {
    let keys: Vec<Retained<NSString>> = pairs.iter().map(|(k, _)| NSString::from_str(k.to_str().unwrap())).collect();
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<&AnyObject> = pairs.iter().map(|(_, v)| *v).collect();
    NSDictionary::from_slices(&keys, &values)
}

/// Reads a fixed-size global property, with an optional i32 qualifier.
unsafe fn get<T>(object: AudioObjectID, selector: AudioObjectPropertySelector, qualifier: Option<i32>) -> Result<T> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size = size_of::<T>() as u32;
    let mut out = MaybeUninit::<T>::uninit();
    let (q_size, q_ptr) = match &qualifier {
        Some(q) => (size_of::<i32>() as u32, q as *const i32 as *const c_void),
        None => (0, std::ptr::null()),
    };
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            q_size,
            q_ptr,
            NonNull::from(&mut size),
            NonNull::new(out.as_mut_ptr() as *mut c_void).unwrap(),
        )
    };
    check(status, "read an audio property")?;
    Ok(unsafe { out.assume_init() })
}

fn check(status: i32, what: &str) -> Result<()> {
    if status != 0 {
        let code = status.to_be_bytes();
        let fourcc = if code.iter().all(|c| c.is_ascii_graphic()) { String::from_utf8_lossy(&code).into_owned() } else { status.to_string() };
        bail!(
            "couldn't {what} (Core Audio error {fourcc}). heyListen needs macOS 14.4+ and permission to record system audio: \
             System Settings → Privacy & Security → Screen & System Audio Recording → enable your terminal app"
        );
    }
    Ok(())
}
