//! Just enough ffi for macOS
//!
//! https://developer.apple.com/documentation/visionkit?language=objc

use std::ptr;

use icrate::{
    objc2::{
        class, extern_class, extern_methods, msg_send, msg_send_id,
        rc::{Allocated, Id, Shared},
        ClassType,
    },
    Foundation::{NSArray, NSDictionary, NSError, NSObject, NSString, NSURL},
};

use crate::core_graphics::CGImageRef;

#[link(name = "Vision", kind = "framework")]
extern "C" {}

extern_class!(
    #[derive(Debug)]
    pub struct VNImageRequestHandler;

    unsafe impl ClassType for VNImageRequestHandler {
        type Super = NSObject;
        const NAME: &'static str = "VNImageRequestHandler";
    }
);

extern_methods!(
    // https://developer.apple.com/documentation/vision/vnimagerequesthandler?language=objc
    #[allow(non_snake_case)]
    unsafe impl VNImageRequestHandler {
        #[method_id(@__retain_semantics Init initWithCGImage:options:)]
        unsafe fn initWithCGImage_options(
            this: Option<Allocated<Self>>,
            image: CGImageRef,
            options: &NSDictionary,
        ) -> Id<Self, Shared>;

        #[method_id(@__retain_semantics Init initWithURL:options:)]
        unsafe fn initWithURL_options(
            this: Option<Allocated<Self>>,
            url: &NSURL,
            options: &NSDictionary,
        ) -> Id<Self, Shared>;
    }
);

// convienence methods
impl VNImageRequestHandler {
    pub fn new_with_cgimage(image: CGImageRef, options: &NSDictionary) -> Id<Self, Shared> {
        unsafe { Self::initWithCGImage_options(Self::alloc(), image, options) }
    }

    pub fn new_with_url(url: &NSURL, options: &NSDictionary) -> Id<Self, Shared> {
        unsafe { Self::initWithURL_options(Self::alloc(), url, options) }
    }

    // FIXME: This is actually a NSArray<VNRequest>
    pub fn perform(&self, requests: &NSArray<VNRecognizeTextRequest>) -> Result<(), NSError> {
        let error: *mut *mut NSError = &mut ptr::null_mut();
        let result: bool = unsafe { msg_send![self, performRequests: requests error: error] };
        if result {
            Ok(())
        } else {
            let error = unsafe { &**error };
            println!("error: {:?}", error.localizedDescription());
            todo!()
        }
    }
}

/*
extern_class!(
    #[derive(Debug)]
    pub struct VNRequest;

    unsafe impl ClassType for VNRequest {
        type Super = NSObject;
        const NAME: &'static str = "VNRequest";
    }
);

extern_class!(
    #[derive(Debug)]
    pub struct VNImageBasedRequest;

    unsafe impl ClassType for VNImageBasedRequest {
        #[inherits(NSObject)]
        type Super = VNRequest;
        const NAME: &'static str = "VNImageBasedRequest";
    }
);
*/

extern_class!(
    #[derive(Debug)]
    pub struct VNRecognizeTextRequest;

    unsafe impl ClassType for VNRecognizeTextRequest {
        //#[inherits(VNRequest, NSObject)]
        //type Super = VNImageBasedRequest;
        type Super = NSObject;
        const NAME: &'static str = "VNRecognizeTextRequest";
    }
);

extern_methods!(
    unsafe impl VNRecognizeTextRequest {
        pub fn new() -> Id<Self, Shared> {
            unsafe { msg_send_id![class!(VNRecognizeTextRequest), new] }
        }

        pub fn languages(&self) -> Id<NSArray<NSString>, Shared> {
            unsafe { msg_send_id![self, recognitionLanguages] }
        }

        pub fn set_languages(&self, languages: &NSArray<NSString>) {
            unsafe { msg_send![self, setRecognitionLanguages: languages] }
        }

        pub fn supported_languages(&self) -> Id<NSArray<NSString>, Shared> {
            let error: *mut *mut NSError = ptr::null_mut();
            unsafe { msg_send_id![self, supportedRecognitionLanguagesAndReturnError: error] }
        }

        pub fn results(&self) -> Id<NSArray<VNRecognizedTextObservation>, Shared> {
            unsafe { msg_send_id![self, results] }
        }
    }
);

extern_class!(
    #[derive(Debug)]
    pub struct VNRecognizedTextObservation;

    unsafe impl ClassType for VNRecognizedTextObservation {
        // #[inherits(VNRectangleObservation, NSObject)]
        type Super = NSObject;
        const NAME: &'static str = "VNRecognizedTextObservation";
    }
);

extern_methods!(
    unsafe impl VNRecognizedTextObservation {
        pub fn top_candidates(
            &self,
            candidate_count: usize,
        ) -> Id<NSArray<VNRecognizedText>, Shared> {
            unsafe { msg_send_id![self, topCandidates: candidate_count] }
        }
    }
);

extern_class!(
    #[derive(Debug)]
    pub struct VNRecognizedText;

    unsafe impl ClassType for VNRecognizedText {
        type Super = NSObject;
        const NAME: &'static str = "VNRecognizedText";
    }
);

extern_methods!(
    unsafe impl VNRecognizedText {
        pub fn string(&self) -> Id<NSString, Shared> {
            unsafe { msg_send_id![self, string] }
        }

        pub fn confidence(&self) -> f32 {
            unsafe { msg_send![self, confidence] }
        }
    }
);
