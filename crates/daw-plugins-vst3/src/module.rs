//! Loading `.vst3` modules and reading their factories.

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use libloading::os::unix::Library;
use vst3::ComPtr;
use vst3::Steinberg::{
    IPluginFactory, IPluginFactory2, IPluginFactory2Trait, IPluginFactory3, IPluginFactory3Trait,
    IPluginFactoryTrait, PClassInfo, PClassInfo2, TUID, kResultOk,
};

use crate::util::cstr_field;

type GetFactoryFn = unsafe extern "system" fn() -> *mut IPluginFactory;
type ModuleEntryFn = unsafe extern "system" fn(*mut c_void) -> bool;
type ModuleExitFn = unsafe extern "system" fn() -> bool;

/// Category of processor classes in a VST3 factory (`kVstAudioEffectClass`).
pub const AUDIO_EFFECT_CLASS: &str = "Audio Module Class";

/// A loaded VST3 module, shared by every instance created from it.
///
/// A module's code stays mapped until the process exits, as in most hosts. Unloading it
/// would race a concurrent load of the same module (the SDK's ModuleEntry and ModuleExit keep
/// a non-atomic counter and a global factory), and plugins leave handlers in the
/// process-wide run loop that would then point into unmapped code. Plugin scans, which load
/// many modules, run in a child process. [`shutdown`] runs ModuleExit before the process
/// exits.
pub struct Module {
    // Released before ModuleExit runs in `Drop`.
    factory: Option<ComPtr<IPluginFactory>>,
    /// Set when ModuleEntry ran, so ModuleExit pairs with it.
    exit: Option<ModuleExitFn>,
}

/// Loaded modules by canonical path. Held while a module is entered or exited, so ModuleEntry
/// and ModuleExit never run concurrently.
static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<Module>>>> = OnceLock::new();

fn cache() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Arc<Module>>> {
    CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Runs ModuleExit for every module no instance uses any more and forgets it, so a later
/// load enters it again. The code stays mapped. Call it once all instances are dropped,
/// before the process exits: plugins (JUCE ones, for example) stop their own threads there.
pub fn shutdown() {
    // Only the cache and instances hold a module, and the cache hands out new references
    // only under this lock, so a count of one cannot grow while it is held. Dropping the
    // module here runs ModuleExit under the lock.
    cache().retain(|_, m| Arc::strong_count(m) > 1);
}

/// One class exported by a factory.
#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub cid: TUID,
    pub category: String,
    pub name: String,
    pub vendor: String,
    pub sub_categories: String,
}

impl Module {
    /// Loads (or reuses) the module at `path`, which may be a `.vst3` bundle directory or a
    /// shared object.
    pub fn load(path: &Path) -> Result<Arc<Module>, String> {
        let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut cache = cache();
        if let Some(module) = cache.get(&key) {
            return Ok(module.clone());
        }
        let module = Arc::new(Self::open(&key)?);
        cache.insert(key, module.clone());
        Ok(module)
    }

    fn open(path: &Path) -> Result<Module, String> {
        let binary = binary_path(path)?;
        let fail = |what: &dyn std::fmt::Display| format!("{}: {what}", binary.display());
        // SAFETY: loading a plugin runs its initializers; that is the point of hosting it.
        let library = unsafe { Library::open(Some(&binary), libc::RTLD_NOW | libc::RTLD_LOCAL) }
            .map_err(|e| fail(&e))?;

        unsafe {
            // Looked up before ModuleEntry, so a module without a factory is never entered.
            let get_factory = library
                .get::<GetFactoryFn>(b"GetPluginFactory\0")
                .map(|f| *f)
                .map_err(|e| fail(&e))?;
            let entry = library.get::<ModuleEntryFn>(b"ModuleEntry\0").map(|f| *f);
            let exit = match entry {
                Ok(_) => library
                    .get::<ModuleExitFn>(b"ModuleExit\0")
                    .ok()
                    .map(|f| *f),
                Err(_) => None,
            };
            // Never unloaded (see `Module`), so the symbols above stay valid.
            let handle = library.into_raw();
            if let Ok(entry) = entry
                && !entry(handle)
            {
                drop(Library::from_raw(handle));
                return Err(fail(&"ModuleEntry failed"));
            }
            let Some(factory) = ComPtr::from_raw(get_factory()) else {
                // Nothing from the module is in use, so it can go entirely.
                if let Some(exit) = exit {
                    exit();
                }
                drop(Library::from_raw(handle));
                return Err(fail(&"GetPluginFactory returned null"));
            };
            // Plugins built with SDK 3.7+ pick up the host's run loop (among other things)
            // from here, before any class is created.
            if let Some(f3) = factory.cast::<IPluginFactory3>() {
                f3.setHostContext(crate::host::host_context());
            }
            Ok(Module {
                factory: Some(factory),
                exit,
            })
        }
    }

    pub fn factory(&self) -> &ComPtr<IPluginFactory> {
        self.factory
            .as_ref()
            .expect("factory is set until the module is dropped")
    }

    pub fn classes(&self) -> Vec<ClassInfo> {
        let factory = self.factory();
        let factory2 = factory.cast::<IPluginFactory2>();
        let count = unsafe { factory.countClasses() };
        let mut out = Vec::new();
        for i in 0..count {
            unsafe {
                if let Some(f2) = &factory2 {
                    let mut info: PClassInfo2 = std::mem::zeroed();
                    if f2.getClassInfo2(i, &mut info) == kResultOk {
                        out.push(ClassInfo {
                            cid: info.cid,
                            category: cstr_field(&info.category),
                            name: cstr_field(&info.name),
                            vendor: cstr_field(&info.vendor),
                            sub_categories: cstr_field(&info.subCategories),
                        });
                        continue;
                    }
                }
                let mut info: PClassInfo = std::mem::zeroed();
                if factory.getClassInfo(i, &mut info) == kResultOk {
                    out.push(ClassInfo {
                        cid: info.cid,
                        category: cstr_field(&info.category),
                        name: cstr_field(&info.name),
                        vendor: String::new(),
                        sub_categories: String::new(),
                    });
                }
            }
        }
        out
    }

    pub fn factory_vendor(&self) -> String {
        unsafe {
            let mut info: vst3::Steinberg::PFactoryInfo = std::mem::zeroed();
            if self.factory().getFactoryInfo(&mut info) == kResultOk {
                cstr_field(&info.vendor)
            } else {
                String::new()
            }
        }
    }
}

impl Drop for Module {
    /// Only [`shutdown`] drops a module, with the cache locked. The library is not closed.
    fn drop(&mut self) {
        self.factory = None;
        if let Some(exit) = self.exit {
            unsafe { exit() };
        }
    }
}

/// Where the shared object for a VST3 module lives.
///
/// A bundle `Foo.vst3/` keeps it at `Foo.vst3/Contents/<arch>-linux/Foo.so`. A bare `.so`
/// (or a single-file `.vst3`) is loaded as is.
pub fn binary_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if !path.is_dir() {
        return Err(format!("{}: not found", path.display()));
    }
    let arch_dir = path.join("Contents").join(format!("{}-linux", arch_name()));
    let stem = path
        .file_stem()
        .ok_or_else(|| format!("{}: bad bundle name", path.display()))?;
    let preferred = arch_dir.join(stem).with_extension("so");
    if preferred.is_file() {
        return Ok(preferred);
    }
    // Some bundles name the binary differently from the bundle.
    std::fs::read_dir(&arch_dir)
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|e| e == "so"))
        })
        .ok_or_else(|| format!("{}: no binary for {}", path.display(), arch_name()))
}

fn arch_name() -> &'static str {
    match std::env::consts::ARCH {
        "x86" => "i386",
        "arm" => "armv7l",
        other => other,
    }
}
