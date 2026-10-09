//! Loading one `.vst3` bundle and getting at its plugin factory.
//!
//! On macOS a VST3 plugin is a bundle whose executable exports three C symbols:
//! `bundleEntry` / `bundleExit` (the module lifecycle) and `GetPluginFactory`
//! (the class factory). The bundle is loaded through `CFBundle` rather than a
//! bare `dlopen` because `bundleEntry` is *handed the `CFBundleRef`* — that is
//! how a plugin locates its own resources (JUCE-based plugins in particular
//! rely on it), and a null there leaves them unable to find their UI assets.
//!
//! [`Vst3Module`] is the loaded bundle plus its factory, and there are two ways
//! to get one, with deliberately opposite lifetimes:
//!
//! - [`with_module`] loads, hands it to a closure, and **unloads** — the
//!   catalog scan, where that unload is what keeps process exit alive.
//! - [`cached_module`] loads and **keeps it forever** — instantiation, where the
//!   module is the code the instance runs and must outlive it.
//!
//! See `docs/180-vst3-host.md`.

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use objc2_core_foundation::{CFBundle, CFRetained, CFString, CFURL, CFURLPathStyle};
use vst3::ComPtr;
use vst3::Steinberg::IPluginFactory;

/// `bool bundleEntry(CFBundleRef)` — the macOS module entry point every `.vst3`
/// bundle exports. Called once per bundle, before the factory is fetched.
type BundleEntryFn = unsafe extern "C" fn(*mut c_void) -> bool;

/// `IPluginFactory* GetPluginFactory()` — the class factory every VST3 module
/// exports. The returned pointer is already owned by the caller (the plugin
/// does the `addRef`), so it is adopted directly into a [`ComPtr`].
type GetFactoryFn = unsafe extern "C" fn() -> *mut IPluginFactory;

/// `bool bundleExit()` — the matching teardown for [`BundleEntryFn`]. The
/// module refcounts the pair, so calling it once per `bundleEntry` is correct.
type BundleExitFn = unsafe extern "C" fn() -> bool;

/// One loaded VST3 bundle: the `CFBundle` keeping its executable mapped, plus
/// the plugin factory every class is created through.
///
/// Cheap to clone — both halves are refcounted handles, which is how a cached
/// module is handed out of [`cached_module`].
#[derive(Clone)]
pub(super) struct Vst3Module {
    /// The loaded bundle — it keeps the executable mapped, and its
    /// `CFBundleRef` is what `bundleEntry` was handed so the plugin can find
    /// its own resources.
    bundle: CFRetained<CFBundle>,
    /// The module's class factory.
    factory: ComPtr<IPluginFactory>,
}

impl Vst3Module {
    /// The module's plugin factory, for enumerating and instantiating classes.
    pub(super) fn factory(&self) -> &ComPtr<IPluginFactory> {
        &self.factory
    }

    /// Releases the factory and runs `bundleExit`, letting the module tear its
    /// statics down now rather than at process exit. Only sound when this is
    /// the only handle to the module and nothing was instantiated from it —
    /// which is why it is private to [`with_module`].
    fn unload(self) {
        let Vst3Module { bundle, factory } = self;
        drop(factory);
        // SAFETY: `bundleExit` pairs with the `bundleEntry` `load_module` ran
        // on this same bundle, and the factory reference taken there was just
        // released. A bundle that somehow exports no `bundleExit` is simply
        // left loaded.
        unsafe {
            if let Some(exit) = function_pointer(&bundle, "bundleExit") {
                let exit: BundleExitFn = std::mem::transmute(exit);
                exit();
            }
            bundle.unload_executable();
        }
    }
}

/// Process-wide cache of loaded VST3 modules, keyed by bundle path — the
/// **load** path's counterpart to [`with_module`], and the exact opposite
/// policy.
///
/// The asymmetry is the whole point. A scan reads metadata and is done, so it
/// unloads (see [`with_module`] for why that is load-bearing). An instantiated
/// plugin, by contrast, *is* code in that module: the module must stay mapped
/// for at least as long as the instance lives, and `bundleExit` must never run
/// underneath it. Since there is no point in the app's life at which we can
/// prove no instance from a bundle is still alive — the same reasoning that
/// leaves still-loaded voices to leak at app exit, see `HostShutdown` — a
/// module that has been loaded for instantiation is simply never unloaded.
///
/// This mirrors `clap::discovery`'s `ENTRY_CACHE`, and shares its other
/// benefit: several plugin instances from one bundle share one module, and the
/// second instrument loaded from a bundle skips straight to `createInstance`.
static MODULE_CACHE: LazyLock<Mutex<HashMap<PathBuf, Vst3Module>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// SAFETY: both fields are already `Send`. `ComPtr<IPluginFactory>` is `Send`
// because the `vst3` bindings assert `IPluginFactory: Send + Sync` (a VST3
// factory carries no thread affinity — it is a plain COM object with atomic
// refcounts), and `CFRetained<CFBundle>` is a refcounted CoreFoundation handle.
// The declaration is needed because the struct is stored in a process-wide
// `static`; access to that static is serialised by its `Mutex` regardless.
unsafe impl Send for Vst3Module {}

/// Returns the cached module for `bundle_path`, loading and caching it on first
/// request. Used by the **load** path only — see [`MODULE_CACHE`].
///
/// Called on the eframe main thread, which matters: some plugins' `bundleEntry`
/// requires it (see [`with_module`]).
pub(super) fn cached_module(bundle_path: &Path) -> Result<Vst3Module, String> {
    let mut cache = MODULE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(module) = cache.get(bundle_path) {
        return Ok(module.clone());
    }
    let module = load_module(bundle_path)?;
    cache.insert(bundle_path.to_path_buf(), module.clone());
    Ok(module)
}

/// Loads `bundle_path`'s module, runs `f` against it, and unloads it again.
///
/// **The unload is not a tidiness measure — it is what keeps app exit alive.**
/// A catalog scan touches every installed bundle, and `bundleEntry` is not a
/// cheap metadata read: a big plugin spins up its real runtime there. Kontakt,
/// Komplete Kontrol, Massive X and Reaktor were each observed starting worker
/// thread pools (`boost::asio`, `spdlog`) during a scan. Leave ~60 of those
/// loaded and `exit()` eventually runs all their static destructors underneath
/// their own still-running threads, in an order nobody has ever tested — which
/// aborts in `__cxa_finalize_ranges` with a C++ exception escaping a
/// destructor. Calling `bundleExit` as soon as the metadata is read lets each
/// module tear itself down while its runtime is still healthy.
///
/// This is sound only because the scan instantiates nothing: the module is the
/// caller's alone and no plugin object outlives `f`. Instantiating a plugin
/// needs a module that stays loaded for as long as the instance lives — a
/// different lifetime, which gets its own entry point.
pub(super) fn with_module<T>(
    bundle_path: &Path,
    f: impl FnOnce(&Vst3Module) -> T,
) -> Result<T, String> {
    let module = load_module(bundle_path)?;
    let out = f(&module);
    module.unload();
    Ok(out)
}

/// Loads one `.vst3` bundle: map its executable, run `bundleEntry`, and adopt
/// the factory `GetPluginFactory` returns.
fn load_module(bundle_path: &Path) -> Result<Vst3Module, String> {
    let path = bundle_path.to_str().ok_or_else(|| {
        format!(
            "vst3: bundle path is not valid UTF-8: {}",
            bundle_path.display()
        )
    })?;
    let url = CFURL::with_file_system_path(
        None,
        Some(&CFString::from_str(path)),
        CFURLPathStyle::CFURLPOSIXPathStyle,
        true,
    )
    .ok_or_else(|| format!("vst3: could not build a URL for {path}"))?;

    let bundle = CFBundle::new(None, Some(&url))
        .ok_or_else(|| format!("vst3: {path} is not a loadable bundle"))?;

    // SAFETY: `bundle` is a live `CFBundle` we just created; loading its
    // executable is exactly what this call is for. We trust the `.vst3`
    // bundles installed on this machine to be well-formed VST3 modules — the
    // same trust `clap::discovery` extends to installed `.clap` bundles.
    if !unsafe { bundle.load_executable() } {
        return Err(format!("vst3: failed to load the executable in {path}"));
    }

    let entry = function_pointer(&bundle, "bundleEntry")
        .ok_or_else(|| format!("vst3: {path} exports no bundleEntry"))?;
    let get_factory = function_pointer(&bundle, "GetPluginFactory")
        .ok_or_else(|| format!("vst3: {path} exports no GetPluginFactory"))?;

    // SAFETY: both symbols were just resolved out of this bundle's loaded
    // executable, and the signatures are the ones the VST3 module ABI fixes
    // for them. `bundleEntry` takes the `CFBundleRef` it will keep for
    // resource lookup, so it is handed this bundle and not null. The matching
    // `bundleExit` runs in `unload` (via `with_module`); a module parked in
    // `MODULE_CACHE` deliberately never gets it.
    let factory = unsafe {
        let entry: BundleEntryFn = std::mem::transmute(entry);
        if !entry(CFRetained::as_ptr(&bundle).as_ptr().cast()) {
            return Err(format!("vst3: bundleEntry failed for {path}"));
        }
        let get_factory: GetFactoryFn = std::mem::transmute(get_factory);
        ComPtr::from_raw(get_factory())
    };

    let factory = factory.ok_or_else(|| format!("vst3: {path} returned a null plugin factory"))?;
    Ok(Vst3Module { bundle, factory })
}

/// Resolves one exported symbol out of a loaded bundle, or `None` if the
/// bundle does not export it.
fn function_pointer(bundle: &CFBundle, name: &str) -> Option<*mut c_void> {
    let ptr = bundle.function_pointer_for_name(Some(&CFString::from_str(name)));
    (!ptr.is_null()).then_some(ptr)
}
