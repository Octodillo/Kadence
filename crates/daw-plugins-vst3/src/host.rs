//! COM objects the host hands to plugins: the host context, messages, streams, the component
//! handler, and the real-time parameter and event queues.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_void};
use std::ptr;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};

use vst3::Steinberg::Linux::IRunLoop;
use vst3::Steinberg::Vst::{
    Event, IAttributeList, IAttributeListTrait, IComponentHandler, IComponentHandlerTrait,
    IConnectionPoint, IConnectionPointTrait, IEventList, IEventListTrait, IHostApplication,
    IHostApplicationTrait, IMessage, IMessageTrait, IParamValueQueue, IParamValueQueueTrait,
    IParameterChanges, IParameterChangesTrait, ParamID, ParamValue, String128, TChar,
};
use vst3::Steinberg::{
    FIDString, FUnknown, IBStream, IBStream_::IStreamSeekMode_, IBStreamTrait, TUID, int32, int64,
    kInvalidArgument, kOutOfMemory, kResultFalse, kResultOk, kResultTrue, tresult, uint32,
};
use vst3::{Class, ComWrapper, Interface};

use crate::runloop::{RunLoop, forward_run_loop};
use crate::util::copy_wstr;

/// Name reported to plugins through `IHostApplication::getName`.
pub const HOST_NAME: &str = "DAW";

fn same_iid<I: Interface>(tuid: *const TUID) -> bool {
    !tuid.is_null() && unsafe { *(tuid as *const [u8; 16]) } == I::IID
}

/// Hands out `iid` from `obj` the way `queryInterface` does.
unsafe fn query<C: Class>(obj: ComWrapper<C>, iid: *const TUID, out: *mut *mut c_void) -> tresult
where
    C::Interfaces: vst3::com_scrape_types::MakeHeader<C, ComWrapper<C>>,
{
    let unknown = obj
        .to_com_ptr::<FUnknown>()
        .expect("every class is an FUnknown");
    unsafe {
        let raw = unknown.as_ptr();
        ((*(*raw).vtbl).queryInterface)(raw, iid as *const TUID, out)
    }
}

// ---------------------------------------------------------------------------------------------
// Host context

/// The host context every plugin is initialized with. It also carries the process-wide run
/// loop, which VSTGUI (SDK 3.7 and later) looks up on the host context.
#[derive(Default)]
pub struct HostApplication {
    pub run_loop: RunLoop,
}

impl Class for HostApplication {
    type Interfaces = (IHostApplication, IRunLoop);
}

forward_run_loop!(HostApplication, self => self.run_loop);

/// Held by tests that pump the shared run loop, so they do not fire each other's timers.
#[cfg(test)]
pub static RUN_LOOP_TEST_LOCK: Mutex<()> = Mutex::new(());

/// The shared host context.
pub fn host() -> &'static ComWrapper<HostApplication> {
    static HOST: OnceLock<ComWrapper<HostApplication>> = OnceLock::new();
    HOST.get_or_init(|| ComWrapper::new(HostApplication::default()))
}

/// The host context as the `FUnknown*` that `initialize` and `setHostContext` take.
pub fn host_context() -> *mut FUnknown {
    host()
        .as_com_ref::<FUnknown>()
        .map_or(ptr::null_mut(), |r| r.as_ptr())
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kInvalidArgument;
        }
        copy_wstr(HOST_NAME, unsafe { &mut *name });
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if obj.is_null() {
            return kInvalidArgument;
        }
        unsafe {
            *obj = ptr::null_mut();
            if same_iid::<IMessage>(cid) {
                query(ComWrapper::new(Message::default()), iid, obj)
            } else if same_iid::<IAttributeList>(cid) {
                query(ComWrapper::new(AttributeList::default()), iid, obj)
            } else {
                kResultFalse
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Messages and attribute lists, used by split processor/controller pairs to talk to each other.

#[derive(Clone)]
enum Attr {
    Int(i64),
    Float(f64),
    String(Vec<TChar>),
    Binary(Vec<u8>),
}

#[derive(Default)]
pub struct AttributeList {
    attrs: Mutex<HashMap<CString, Attr>>,
}

impl AttributeList {
    fn set(&self, id: FIDString, value: Attr) -> tresult {
        if id.is_null() {
            return kInvalidArgument;
        }
        let key = unsafe { CStr::from_ptr(id) }.to_owned();
        self.attrs.lock().unwrap().insert(key, value);
        kResultOk
    }

    fn with<R>(&self, id: FIDString, f: impl FnOnce(&Attr) -> Option<R>) -> Option<R> {
        if id.is_null() {
            return None;
        }
        let key = unsafe { CStr::from_ptr(id) };
        self.attrs.lock().unwrap().get(key).and_then(f)
    }
}

impl Class for AttributeList {
    type Interfaces = (IAttributeList,);
}

impl IAttributeListTrait for AttributeList {
    unsafe fn setInt(&self, id: FIDString, value: int64) -> tresult {
        self.set(id, Attr::Int(value))
    }

    unsafe fn getInt(&self, id: FIDString, value: *mut int64) -> tresult {
        match self.with(id, |a| match a {
            Attr::Int(v) => Some(*v),
            _ => None,
        }) {
            Some(v) if !value.is_null() => {
                unsafe { *value = v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: FIDString, value: f64) -> tresult {
        self.set(id, Attr::Float(value))
    }

    unsafe fn getFloat(&self, id: FIDString, value: *mut f64) -> tresult {
        match self.with(id, |a| match a {
            Attr::Float(v) => Some(*v),
            _ => None,
        }) {
            Some(v) if !value.is_null() => {
                unsafe { *value = v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: FIDString, string: *const TChar) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut s = Vec::new();
        let mut i = 0;
        unsafe {
            while *string.add(i) != 0 {
                s.push(*string.add(i));
                i += 1;
            }
        }
        s.push(0);
        self.set(id, Attr::String(s))
    }

    unsafe fn getString(
        &self,
        id: FIDString,
        string: *mut TChar,
        size_in_bytes: uint32,
    ) -> tresult {
        let capacity = size_in_bytes as usize / std::mem::size_of::<TChar>();
        if string.is_null() || capacity == 0 {
            return kInvalidArgument;
        }
        let found = self.with(id, |a| match a {
            Attr::String(s) => {
                let n = s.len().min(capacity);
                unsafe {
                    ptr::copy_nonoverlapping(s.as_ptr(), string, n);
                    *string.add(n - 1) = 0;
                }
                Some(())
            }
            _ => None,
        });
        if found.is_some() {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn setBinary(
        &self,
        id: FIDString,
        data: *const c_void,
        size_in_bytes: uint32,
    ) -> tresult {
        let bytes = if data.is_null() || size_in_bytes == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(data as *const u8, size_in_bytes as usize) }
                .to_vec()
        };
        self.set(id, Attr::Binary(bytes))
    }

    unsafe fn getBinary(
        &self,
        id: FIDString,
        data: *mut *const c_void,
        size_in_bytes: *mut uint32,
    ) -> tresult {
        if data.is_null() || size_in_bytes.is_null() {
            return kInvalidArgument;
        }
        // The pointer stays valid until the attribute is replaced or the list is released,
        // which is what the API promises.
        let found = self.with(id, |a| match a {
            Attr::Binary(b) => Some((b.as_ptr() as *const c_void, b.len() as uint32)),
            _ => None,
        });
        match found {
            Some((p, n)) => {
                unsafe {
                    *data = p;
                    *size_in_bytes = n;
                }
                kResultOk
            }
            None => kResultFalse,
        }
    }
}

pub struct Message {
    id: Mutex<CString>,
    attributes: ComWrapper<AttributeList>,
}

impl Default for Message {
    fn default() -> Self {
        Self {
            id: Mutex::new(CString::default()),
            attributes: ComWrapper::new(AttributeList::default()),
        }
    }
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        // The CString's heap buffer does not move until the next setMessageID.
        self.id.lock().unwrap().as_ptr()
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        let id = if id.is_null() {
            CString::default()
        } else {
            unsafe { CStr::from_ptr(id) }.to_owned()
        };
        *self.id.lock().unwrap() = id;
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        // Not reference counted: the message owns its attribute list.
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map_or(ptr::null_mut(), |r| r.as_ptr())
    }
}

// ---------------------------------------------------------------------------------------------
// Byte stream for component and controller state.

/// Largest state a plugin may write, so a garbage seek cannot exhaust memory.
const MAX_STREAM_LEN: usize = i32::MAX as usize;

#[derive(Default)]
struct StreamData {
    data: Vec<u8>,
    pos: usize,
}

#[derive(Default)]
pub struct MemoryStream {
    inner: Mutex<StreamData>,
}

impl MemoryStream {
    pub fn with_data(data: Vec<u8>) -> Self {
        Self {
            inner: Mutex::new(StreamData { data, pos: 0 }),
        }
    }

    pub fn take(&self) -> Vec<u8> {
        let mut inner = self.inner.lock().unwrap();
        inner.pos = 0;
        std::mem::take(&mut inner.data)
    }

    pub fn rewind(&self) {
        self.inner.lock().unwrap().pos = 0;
    }
}

impl Class for MemoryStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(&self, buffer: *mut c_void, num_bytes: int32, num_read: *mut int32) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kInvalidArgument;
        }
        let mut inner = self.inner.lock().unwrap();
        let pos = inner.pos.min(inner.data.len());
        let n = (num_bytes as usize).min(inner.data.len() - pos);
        unsafe { ptr::copy_nonoverlapping(inner.data.as_ptr().add(pos), buffer as *mut u8, n) };
        inner.pos = pos + n;
        if !num_read.is_null() {
            unsafe { *num_read = n as int32 };
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_written: *mut int32,
    ) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kInvalidArgument;
        }
        if !num_written.is_null() {
            unsafe { *num_written = 0 };
        }
        let mut inner = self.inner.lock().unwrap();
        let src = unsafe { std::slice::from_raw_parts(buffer as *const u8, num_bytes as usize) };
        let pos = inner.pos;
        // `seek` accepts any position, so a buggy plugin can ask for an absurd size. Refuse it
        // rather than panic, which would abort through the vtable thunk.
        let Some(end) = pos.checked_add(src.len()).filter(|&e| e <= MAX_STREAM_LEN) else {
            return kOutOfMemory;
        };
        if inner.data.len() < end {
            let grow = end - inner.data.len();
            if inner.data.try_reserve(grow).is_err() {
                return kOutOfMemory;
            }
            inner.data.resize(end, 0);
        }
        inner.data[pos..end].copy_from_slice(src);
        inner.pos = end;
        if !num_written.is_null() {
            unsafe { *num_written = num_bytes };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let mut inner = self.inner.lock().unwrap();
        let base = match mode as vst3::Steinberg::IBStream_::IStreamSeekMode {
            IStreamSeekMode_::kIBSeekSet => 0,
            IStreamSeekMode_::kIBSeekCur => inner.pos as i64,
            IStreamSeekMode_::kIBSeekEnd => inner.data.len() as i64,
            _ => return kInvalidArgument,
        };
        let Some(new) = base.checked_add(pos).filter(|p| *p >= 0) else {
            return kInvalidArgument;
        };
        inner.pos = new as usize;
        if !result.is_null() {
            unsafe { *result = new };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        unsafe { *pos = self.inner.lock().unwrap().pos as int64 };
        kResultOk
    }
}

// ---------------------------------------------------------------------------------------------
// The processor's connection to its controller.

thread_local! {
    /// True while this thread is inside a call that runs on the audio thread (`process`, or
    /// `setProcessing` from `reset`).
    static IN_PROCESS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f`, a call into the processor on the audio thread, with messages the processor
/// sends from it refused.
pub fn in_process<R>(f: impl FnOnce() -> R) -> R {
    IN_PROCESS.with(|p| p.set(true));
    let r = f();
    IN_PROCESS.with(|p| p.set(false));
    r
}

/// The processor's view of its controller. Messages are for the UI thread; one sent from
/// `process` (a meter, say) would run controller code, with its UI locks, on the audio
/// thread. Like the SDK's reference host (`PlugProvider`'s connection proxy), the proxy
/// refuses those with `kResultFalse`. Queuing them instead is not safe: plugins that send
/// from `process` without allocating reuse one message and rewrite it while the main thread
/// would still be reading it. Messages sent from any other call go straight through.
pub struct ControllerProxy {
    controller: vst3::ComPtr<IConnectionPoint>,
}

impl Class for ControllerProxy {
    type Interfaces = (IConnectionPoint,);
}

impl ControllerProxy {
    pub fn new(controller: vst3::ComPtr<IConnectionPoint>) -> Self {
        ControllerProxy { controller }
    }
}

impl IConnectionPointTrait for ControllerProxy {
    unsafe fn connect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }

    unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
        kResultOk
    }

    unsafe fn notify(&self, message: *mut IMessage) -> tresult {
        if IN_PROCESS.with(|p| p.get()) {
            return kResultFalse;
        }
        unsafe { self.controller.notify(message) }
    }
}

// ---------------------------------------------------------------------------------------------
// Component handler: edits coming from the plugin's own editor.

/// A parameter edit made in the plugin's editor, on its way to the processor.
#[derive(Copy, Clone, Debug)]
pub struct Edit {
    pub id: ParamID,
    pub normalized: ParamValue,
}

/// Edits on their way to `process`. When the ring is full, later edits wait in `overflow`,
/// latest value per parameter, until the main thread moves them over. Everything in
/// `overflow` is newer than everything in the ring, so the last edit always wins.
struct EditQueue {
    ring: rtrb::Producer<Edit>,
    overflow: Vec<Edit>,
}

pub struct ComponentHandler {
    edits: Mutex<EditQueue>,
    restart_flags: AtomicI32,
}

impl ComponentHandler {
    pub fn new(edits: rtrb::Producer<Edit>) -> Self {
        Self {
            edits: Mutex::new(EditQueue {
                ring: edits,
                overflow: Vec::new(),
            }),
            restart_flags: AtomicI32::new(0),
        }
    }

    /// Moves edits that did not fit into the ring as room frees up. Main thread only.
    pub fn retry_overflow(&self) {
        let mut q = self.edits.lock().unwrap();
        let EditQueue { ring, overflow } = &mut *q;
        let mut moved = 0;
        while moved < overflow.len() && ring.push(overflow[moved]).is_ok() {
            moved += 1;
        }
        overflow.drain(..moved);
    }

    /// Forgets edits waiting for room in the ring, after a state load made them stale.
    pub fn discard_overflow(&self) {
        self.edits.lock().unwrap().overflow.clear();
    }

    /// Returns and clears the flags passed to `restartComponent` since the last call.
    pub fn take_restart_flags(&self) -> i32 {
        self.restart_flags.swap(0, Ordering::AcqRel)
    }
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn performEdit(&self, id: ParamID, normalized: ParamValue) -> tresult {
        let edit = Edit { id, normalized };
        let mut q = self.edits.lock().unwrap();
        if !q.overflow.is_empty() || q.ring.push(edit).is_err() {
            match q.overflow.iter_mut().find(|e| e.id == id) {
                Some(e) => e.normalized = normalized,
                None => q.overflow.push(edit),
            }
        }
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        self.restart_flags.fetch_or(flags, Ordering::AcqRel);
        kResultOk
    }
}

// ---------------------------------------------------------------------------------------------
// Real-time parameter queues. Built once in `prepare`, then used only from `process` (by the
// host and by the plugin, never at the same time), so they never allocate or lock.

pub struct ParamQueue {
    id: UnsafeCell<ParamID>,
    points: UnsafeCell<Vec<(int32, ParamValue)>>,
}

// SAFETY: only touched from inside one `process` call at a time; see the section comment.
unsafe impl Sync for ParamQueue {}

impl ParamQueue {
    fn new(capacity: usize) -> Self {
        Self {
            id: UnsafeCell::new(0),
            points: UnsafeCell::new(Vec::with_capacity(capacity)),
        }
    }

    #[allow(clippy::mut_from_ref)]
    fn points(&self) -> &mut Vec<(int32, ParamValue)> {
        unsafe { &mut *self.points.get() }
    }

    fn reset(&self, id: ParamID) {
        unsafe { *self.id.get() = id };
        self.points().clear();
    }

    pub fn id(&self) -> ParamID {
        unsafe { *self.id.get() }
    }

    pub fn last(&self) -> Option<(int32, ParamValue)> {
        self.points().last().copied()
    }

    #[cfg(test)]
    pub fn iter(&self) -> impl Iterator<Item = (int32, ParamValue)> + '_ {
        self.points().iter().copied()
    }

    /// Adds a point, keeping offsets sorted and replacing one at the same offset.
    /// Returns the point's index, or `None` when full.
    fn add(&self, offset: int32, value: ParamValue) -> Option<usize> {
        let points = self.points();
        let at = points.partition_point(|(o, _)| *o < offset);
        if points.get(at).is_some_and(|(o, _)| *o == offset) {
            points[at].1 = value;
            return Some(at);
        }
        if points.len() == points.capacity() {
            return None;
        }
        points.insert(at, (offset, value));
        Some(at)
    }
}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id()
    }

    unsafe fn getPointCount(&self) -> int32 {
        self.points().len() as int32
    }

    unsafe fn getPoint(
        &self,
        index: int32,
        sample_offset: *mut int32,
        value: *mut ParamValue,
    ) -> tresult {
        let Some(&(o, v)) = self.points().get(index as usize) else {
            return kResultFalse;
        };
        if sample_offset.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        unsafe {
            *sample_offset = o;
            *value = v;
        }
        kResultTrue
    }

    unsafe fn addPoint(
        &self,
        sample_offset: int32,
        value: ParamValue,
        index: *mut int32,
    ) -> tresult {
        match self.add(sample_offset, value) {
            Some(i) => {
                if !index.is_null() {
                    unsafe { *index = i as int32 };
                }
                kResultTrue
            }
            None => kResultFalse,
        }
    }
}

pub struct ParamChanges {
    queues: Vec<ComWrapper<ParamQueue>>,
    used: UnsafeCell<usize>,
}

// SAFETY: as for `ParamQueue`.
unsafe impl Sync for ParamChanges {}

impl ParamChanges {
    /// Room for `params` distinct parameters with `points` points each per block.
    pub fn new(params: usize, points: usize) -> Self {
        Self {
            queues: (0..params)
                .map(|_| ComWrapper::new(ParamQueue::new(points)))
                .collect(),
            used: UnsafeCell::new(0),
        }
    }

    fn used(&self) -> usize {
        unsafe { *self.used.get() }
    }

    pub fn clear(&self) {
        unsafe { *self.used.get() = 0 };
    }

    pub fn queues(&self) -> impl Iterator<Item = &ParamQueue> {
        self.queues[..self.used()].iter().map(|q| &**q)
    }

    fn queue_for(&self, id: ParamID) -> Option<(usize, &ParamQueue)> {
        let used = self.used();
        if let Some(i) = self.queues[..used].iter().position(|q| q.id() == id) {
            return Some((i, &self.queues[i]));
        }
        let q = self.queues.get(used)?;
        q.reset(id);
        unsafe { *self.used.get() = used + 1 };
        Some((used, q))
    }

    /// Host-side insert. Returns false when out of room.
    pub fn add(&self, id: ParamID, offset: int32, value: ParamValue) -> bool {
        self.queue_for(id)
            .is_some_and(|(_, q)| q.add(offset, value).is_some())
    }

    fn queue_ptr(&self, i: usize) -> *mut IParamValueQueue {
        self.queues[i]
            .as_com_ref::<IParamValueQueue>()
            .map_or(ptr::null_mut(), |r| r.as_ptr())
    }
}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        self.used() as int32
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        if index < 0 || index as usize >= self.used() {
            return ptr::null_mut();
        }
        self.queue_ptr(index as usize)
    }

    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut int32,
    ) -> *mut IParamValueQueue {
        if id.is_null() {
            return ptr::null_mut();
        }
        match self.queue_for(unsafe { *id }) {
            Some((i, _)) => {
                if !index.is_null() {
                    unsafe { *index = i as int32 };
                }
                self.queue_ptr(i)
            }
            None => ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Real-time event list, same threading rules as the parameter queues.

pub struct EventQueue {
    events: UnsafeCell<Vec<Event>>,
}

// SAFETY: as for `ParamQueue`.
unsafe impl Sync for EventQueue {}

impl EventQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            events: UnsafeCell::new(Vec::with_capacity(capacity)),
        }
    }

    #[allow(clippy::mut_from_ref)]
    fn events(&self) -> &mut Vec<Event> {
        unsafe { &mut *self.events.get() }
    }

    pub fn clear(&self) {
        self.events().clear();
    }

    /// Host-side push. Returns false when full.
    pub fn push(&self, e: Event) -> bool {
        let events = self.events();
        if events.len() == events.capacity() {
            return false;
        }
        events.push(e);
        true
    }

    pub fn iter(&self) -> impl Iterator<Item = &Event> {
        self.events().iter()
    }
}

impl Class for EventQueue {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for EventQueue {
    unsafe fn getEventCount(&self) -> int32 {
        self.events().len() as int32
    }

    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        match self.events().get(index as usize) {
            Some(ev) if !e.is_null() => {
                unsafe { *e = *ev };
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        if e.is_null() {
            return kInvalidArgument;
        }
        if self.push(unsafe { *e }) {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vst3::ComPtr;

    #[test]
    fn stream_reads_back_what_was_written() {
        let s = ComWrapper::new(MemoryStream::default());
        let p: ComPtr<IBStream> = s.to_com_ptr().unwrap();
        let mut data = *b"hello";
        let mut n = 0;
        unsafe {
            assert_eq!(p.write(data.as_mut_ptr().cast(), 5, &mut n), kResultOk);
            assert_eq!(n, 5);
            let mut pos = 0;
            p.seek(1, IStreamSeekMode_::kIBSeekSet as i32, &mut pos);
            let mut out = [0u8; 8];
            p.read(out.as_mut_ptr().cast(), 8, &mut n);
            assert_eq!(&out[..n as usize], b"ello");
        }
        assert_eq!(s.take(), b"hello");
    }

    #[test]
    fn writing_far_past_the_end_fails_instead_of_panicking() {
        // Called directly rather than through the vtable, where a panic would abort.
        let s = MemoryStream::default();
        let mut data = *b"x";
        let mut n = 7;
        for at in [i64::MAX - 1, 1 << 40] {
            unsafe {
                assert_eq!(
                    s.seek(at, IStreamSeekMode_::kIBSeekSet as i32, ptr::null_mut()),
                    kResultOk
                );
                assert_ne!(s.write(data.as_mut_ptr().cast(), 1, &mut n), kResultOk);
            }
            assert_eq!(n, 0);
        }
        assert!(s.take().is_empty());
    }

    #[test]
    fn param_queues_merge_by_id_and_sort_points() {
        let pc = ParamChanges::new(2, 3);
        assert!(pc.add(7, 10, 0.5));
        assert!(pc.add(7, 0, 0.1));
        assert!(pc.add(7, 10, 0.6));
        assert!(pc.add(9, 0, 1.0));
        assert!(!pc.add(11, 0, 1.0), "only two queues");
        let q: Vec<_> = pc
            .queues()
            .map(|q| (q.id(), q.iter().collect::<Vec<_>>()))
            .collect();
        assert_eq!(q, vec![(7, vec![(0, 0.1), (10, 0.6)]), (9, vec![(0, 1.0)])]);
        pc.clear();
        assert_eq!(pc.queues().count(), 0);
    }

    #[test]
    fn host_creates_messages_with_attributes() {
        let host = host();
        let mut cid: TUID = unsafe { std::mem::transmute(IMessage::IID) };
        let mut iid = cid;
        let mut obj = ptr::null_mut();
        unsafe {
            assert_eq!(host.createInstance(&mut cid, &mut iid, &mut obj), kResultOk);
            let msg = ComPtr::<IMessage>::from_raw(obj as *mut IMessage).unwrap();
            msg.setMessageID(c"hi".as_ptr());
            assert_eq!(CStr::from_ptr(msg.getMessageID()), c"hi");
            let attrs = vst3::ComRef::from_raw(msg.getAttributes()).unwrap();
            attrs.setFloat(c"x".as_ptr(), 2.5);
            let mut v = 0.0;
            assert_eq!(attrs.getFloat(c"x".as_ptr(), &mut v), kResultOk);
            assert_eq!(v, 2.5);
        }
    }

    struct CountingTimer(std::sync::atomic::AtomicU32);

    impl Class for CountingTimer {
        type Interfaces = (vst3::Steinberg::Linux::ITimerHandler,);
    }

    impl vst3::Steinberg::Linux::ITimerHandlerTrait for CountingTimer {
        unsafe fn onTimer(&self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn host_context_offers_the_run_loop() {
        use vst3::Steinberg::Linux::{IRunLoopTrait, ITimerHandler};
        let _pumping = RUN_LOOP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let run_loop = unsafe { vst3::ComRef::<FUnknown>::from_raw(host_context()) }
            .and_then(|h| h.cast::<IRunLoop>())
            .expect("IRunLoop on the host context");
        let timer = ComWrapper::new(CountingTimer(Default::default()));
        let handler = timer.as_com_ref::<ITimerHandler>().unwrap().as_ptr();
        unsafe {
            assert_eq!(run_loop.registerTimer(handler, 1), kResultTrue);
            std::thread::sleep(std::time::Duration::from_millis(5));
            host().run_loop.pump();
            assert_eq!(run_loop.unregisterTimer(handler), kResultTrue);
        }
        host().run_loop.pump();
        assert_eq!(timer.0.load(Ordering::Relaxed), 1);
    }
}
