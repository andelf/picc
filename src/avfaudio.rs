use std::ptr;

use icrate::{
    block2::ConcreteBlock,
    objc2::{
        extern_class, msg_send, msg_send_id,
        rc::{Id, Owned, Shared},
        ClassType,
    },
    Foundation::{NSError, NSObject},
};

#[link(name = "AVFoundation", kind = "framework")]
extern "C" {}

extern_class!(
    #[derive(Debug)]
    pub struct AVAudioInputNode;

    unsafe impl ClassType for AVAudioInputNode {
        type Super = NSObject;
        const NAME: &'static str = "AVAudioInputNode";
    }
);

impl AVAudioInputNode {
    pub fn output_format_for_bus(&self, bus: usize) -> Id<AVAudioFormat, Shared> {
        unsafe { msg_send_id![self, outputFormatForBus: bus] }
    }

    pub fn install_tap_on_bus<F>(&self, bus: usize, buffer_size: u32, format: &AVAudioFormat, f: F)
    where
        F: 'static + Fn(&AVAudioPCMBuffer, &AVAudioTime) -> (),
    {
        let block = ConcreteBlock::new(move |buffer: &AVAudioPCMBuffer, time_ref: &AVAudioTime| {
            f(buffer, time_ref);
        })
        .copy();

        unsafe {
            msg_send![self, installTapOnBus: bus bufferSize: buffer_size format: format block: &*block]
        }
    }
}

extern_class!(
    #[derive(Debug)]
    pub struct AVAudioFormat;

    unsafe impl ClassType for AVAudioFormat {
        type Super = NSObject;
        const NAME: &'static str = "AVAudioFormat";
    }
);

extern_class!(
    #[derive(Debug)]
    pub struct AVAudioPCMBuffer;

    unsafe impl ClassType for AVAudioPCMBuffer {
        type Super = NSObject;
        const NAME: &'static str = "AVAudioPCMBuffer";
    }
);

extern_class!(
    #[derive(Debug)]
    pub struct AVAudioTime;

    unsafe impl ClassType for AVAudioTime {
        type Super = NSObject;
        const NAME: &'static str = "AVAudioTime";
    }
);

extern_class!(
    #[derive(Debug)]
    pub struct AVAudioEngine;

    unsafe impl ClassType for AVAudioEngine {
        type Super = NSObject;
        const NAME: &'static str = "AVAudioEngine";
    }
);

impl AVAudioEngine {
    pub fn new() -> Id<Self, Owned> {
        unsafe { msg_send_id![Self::class(), new] }
    }

    pub fn input_node(&self) -> Id<AVAudioInputNode, Shared> {
        unsafe { msg_send_id![self, inputNode] }
    }

    pub fn prepare(&self) {
        unsafe { msg_send![self, prepare] }
    }

    pub fn running(&self) -> bool {
        unsafe { msg_send![self, isRunning] }
    }

    pub fn start(&self) -> Result<(), Id<NSError, Owned>> {
        let err: *mut *mut NSError = ptr::null_mut();
        let success: bool = unsafe { msg_send![self, startAndReturnError: err] };
        if success {
            Ok(())
        } else {
            println!("err => {:?}", err);
            todo!("error type ")
        }
    }
}
