//! Tiny on-disk fixture corpora: stand-ins for absent checkouts, and the
//! synthetic last resort for a corpus that yields no docs. Never real
//! sources — every caller labels their numbers as fixture numbers.

use std::path::PathBuf;

/// Tiny Chromium-shaped fixture under the system temp dir, used when the
/// Chromium checkout is absent. Its files embed the default literal terms so
/// a fixture run still shows hits, and it is rewritten deterministically.
pub fn ensure_chromium_fixture() -> std::path::PathBuf {
    const FILES: &[(&str, &str)] = &[
        (
            "base/memory/ref_counted.cc",
            "#include \"base/memory/ref_counted.h\"\n\
             #include \"base/synchronization/lock.h\"\n\
             \n\
             namespace base {\n\
             \n\
             // Logging hook mirroring kernel printk usage, so the default\n\
             // bench terms hit this fixture too.\n\
             void LogRefCount(const char* tag) {\n\
               printk(\"%s refs acquired\\n\", tag);  // mutex_lock kmalloc\n\
               spin_lock(&g_ref_lock);  // copy_from_user\n\
             }\n\
             \n\
             }  // namespace base\n",
        ),
        (
            "content/browser/browser_context.h",
            "#ifndef CONTENT_BROWSER_BROWSER_CONTEXT_H_\n\
             #define CONTENT_BROWSER_BROWSER_CONTEXT_H_\n\
             \n\
             namespace content {\n\
             \n\
             // Owns the storage partition; teardown takes mutex_lock and\n\
             // releases via kmalloc'd shutdown closures (printk on error).\n\
             class BrowserContext {\n\
              public:\n\
               virtual void ShutdownWhileLocked();  // spin_lock copy_from_user\n\
             };\n\
             \n\
             }  // namespace content\n\
             \n\
             #endif  // CONTENT_BROWSER_BROWSER_CONTEXT_H_\n",
        ),
        (
            "third_party/blink/renderer/core/dom/document.js",
            "// Blink Document binding stub (fixture): dispatch hooks log via\n\
             // printk-style tracing; layout takes mutex_lock/kmalloc paths.\n\
             function onDOMContentLoaded() {\n\
               spin_lock('layout');  // copy_from_user\n\
               console.log('document ready');\n\
             }\n",
        ),
        (
            "ui/views/widget/window.ts",
            "// Views Widget wrapper (fixture): focus changes grab mutex_lock,\n\
             // buffers are kmalloc'd, errors go through printk tracing.\n\
             export class Window {\n\
               focus() { spin_lock('focus'); /* copy_from_user */ }\n\
             }\n",
        ),
    ];
    let dir = std::env::temp_dir().join("fmgrep-chromium-fixture");
    for (rel, content) in FILES {
        let path = dir.join(rel);
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return dir;
        }
        if std::fs::write(&path, content).is_err() {
            return dir;
        }
    }
    dir
}

/// [`ensure_chromium_fixture`] for the `"ghc"` id, with GHC-idiomatic
/// paths (`.hs`/`.lhs`/`.c`/`.h`).
pub fn ensure_ghc_fixture() -> std::path::PathBuf {
    const FILES: &[(&str, &str)] = &[
        (
            "compiler/GHC/Driver/Fixture.hs",
            "module GHC.Driver.Fixture (scheduleWork) where\n\
             \n\
             -- Logging hook mirroring kernel printk usage, so the default\n\
             -- bench terms hit this fixture too.\n\
             scheduleWork :: String -> IO ()\n\
             scheduleWork tag = do\n\
               printk (\"refs acquired: \" ++ tag)  -- mutex_lock kmalloc\n\
               spin_lock g_ref_lock  -- copy_from_user\n",
        ),
        (
            "libraries/ghc-internal/src/GHC/Internal/Fixture.hs",
            "module GHC.Internal.Fixture (shutdownWhileLocked) where\n\
             \n\
             -- Teardown takes mutex_lock and releases via kmalloc'd shutdown\n\
             -- closures (printk on error).\n\
             shutdownWhileLocked :: IO ()\n\
             shutdownWhileLocked = spin_lock \"shutdown\"  -- copy_from_user\n",
        ),
        (
            "testsuite/tests/driver/Fixture.lhs",
            "> module Fixture (onTestDone) where\n\
             >\n\
             > -- Bird-track literate stub (fixture): completion hooks log via\n\
             > -- printk-style tracing; setup takes mutex_lock/kmalloc paths.\n\
             > onTestDone :: IO ()\n\
             > onTestDone = spin_lock \"layout\"  -- copy_from_user\n",
        ),
        (
            "runtime/Fixture.c",
            "/* C wrapper stub (fixture): focus changes grab mutex_lock,\n\
             * buffers are kmalloc'd, errors go through printk tracing. */\n\
             #include \"Fixture.h\"\n\
             void fixture_focus(void) {\n\
                 spin_lock(&fixture_lock); /* copy_from_user */\n\
             }\n",
        ),
        (
            "includes/Fixture.h",
            "#ifndef FIXTURE_H\n\
             #define FIXTURE_H\n\
             \n\
             /* Owns the fixture lock; teardown takes mutex_lock (printk on\n\
              * error) and releases kmalloc'd state. spin_lock copy_from_user. */\n\
             void fixture_focus(void);\n\
             \n\
             #endif /* FIXTURE_H */\n",
        ),
    ];
    let dir = std::env::temp_dir().join("fmgrep-ghc-fixture");
    for (rel, content) in FILES {
        let path = dir.join(rel);
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return dir;
        }
        if std::fs::write(&path, content).is_err() {
            return dir;
        }
    }
    dir
}

/// Two synthetic docs on disk for a corpus with nothing to load, so the
/// runner still has a real tree to describe.
pub fn ensure_synthetic_fixture() -> Result<PathBuf, String> {
    const FILES: &[(&str, &str)] = &[
        (
            "a.c",
            "printk hello mutex_lock kmalloc spin_lock copy_from_user\n",
        ),
        ("b.c", "printk again and again printk\n"),
    ];
    let dir = std::env::temp_dir().join("fmgrep-synthetic-fixture");
    let sub = dir.join("synthetic");
    std::fs::create_dir_all(&sub).map_err(|e| format!("{}: {e}", sub.display()))?;
    for (name, content) in FILES {
        let path = sub.join(name);
        std::fs::write(&path, content).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(dir)
}
