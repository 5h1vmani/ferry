package app.ferry

import android.content.Context
import app.ferry.engine.FerryEngine
import java.io.File
import uniffi.ferry_runtime.BatchInfo
import uniffi.ferry_runtime.TransferState as EngineTransferState

// Which cache copies ShareIntake made belong to which batch, and clearing
// them once that batch no longer needs them. Split from ShareIntake.kt,
// docs/audits/principles.md row P11.
//
// ShareIntake decides what needs a cache copy and makes it. This object
// only tracks and clears those copies, by the batch id FerryEngine hands
// back once a push has one.
//
// Rule, docs/audits/principles-fixes.md row 6. Two passes clear entries,
// and each may delete only what it can be sure is safe to delete.
// sweepOrphaned runs once, when the engine starts. At that moment nothing
// from a past run of this app can still be in use, so it may delete every
// entry no live batch names and every cache folder no surviving entry
// names. reconcile runs again on each change to the batches list, while
// this run is live, so it deletes only entries whose own batch is Done.
// It never treats an entry with no matching batch as orphaned, because a
// share this run just started is registered under a request id before
// the engine hands back a real batch id, and that id will not match any
// batch yet.
object ShareCacheRegistry {
    private const val REGISTRY_SUBDIR = "share_cache_registry"

    // What the registry and the cache already held the moment this run of
    // the app started, taken before anything this run does can add to
    // either. FerryApplication.onCreate calls ShareIntake.start, which
    // takes this snapshot, before any activity exists and before any
    // share can be resolved. sweepOrphaned below only ever deletes a name
    // in this snapshot, so a request id a live push registers after start
    // can never look orphaned to it, no matter how long the sweep waits
    // for the engine to finish starting.
    data class StartSnapshot(val registryNames: Set<String>, val cacheDirNames: Set<String>)

    // Called once, synchronously, from ShareIntake.start, before that
    // function launches the coroutine that later waits for the engine and
    // runs the sweep.
    fun snapshotAtStart(context: Context): StartSnapshot {
        val registryNames = File(context.filesDir, REGISTRY_SUBDIR).listFiles().orEmpty()
            .map { it.name }.toSet()
        val cacheDirNames = File(context.cacheDir, ShareIntake.CACHE_SUBDIR).listFiles().orEmpty()
            .map { it.name }.toSet()
        return StartSnapshot(registryNames, cacheDirNames)
    }

    // True for a path ShareIntake copied into its cache, as opposed to a
    // path resolved straight from MediaStore, which is never deleted here.
    private fun isCacheCopy(context: Context, path: String): Boolean =
        path.startsWith(File(context.cacheDir, ShareIntake.CACHE_SUBDIR).absolutePath)

    // Records which cache copies belong to one share, under an id the
    // caller already has, so they can be deleted once that id names a
    // batch that reaches Done. Audit finding 7: called with a request id
    // before the push, not only after with the real batch id, since a
    // push that never returns one must not lose track of its copies.
    // A call with no cache copies writes nothing.
    fun registerBatch(context: Context, id: String, localPaths: List<String>) {
        val cachePaths = localPaths.filter { isCacheCopy(context, it) }
        if (cachePaths.isEmpty()) {
            return
        }
        val registryDir = File(context.filesDir, REGISTRY_SUBDIR)
        registryDir.mkdirs()
        File(registryDir, id).writeText(cachePaths.joinToString("\n"))
    }

    // Moves a registration from the request id it was made under to the
    // real batch id pushFiles returned, once it succeeds: audit finding 7.
    fun renameRegistration(context: Context, fromId: String, toId: String) {
        val registryDir = File(context.filesDir, REGISTRY_SUBDIR)
        val from = File(registryDir, fromId)
        if (from.exists()) {
            from.renameTo(File(registryDir, toId))
        }
    }

    // Removes every cache copy whose batch is Done, or whose batch no
    // engine record names any more: docs/ux-fix-plan.md item 1, "at app
    // start when no transfer names it". Also removes every folder under
    // the cache itself that no surviving registry entry names: audit
    // finding 7. A push that threw before pushFiles returned a batch id
    // left its copy registered only under a request id no batch will ever
    // match, and the registry-only sweep above never looked at the cache
    // folder to find it.
    //
    // Only a name in atStart is ever looked at here: docs/audits/
    // principles-fixes.md row 6. A name written after the snapshot was
    // taken belongs to a share this run is making, and this pass leaves
    // it alone. reconcile clears it later, once it is a real batch id and
    // that batch reaches Done.
    fun sweepOrphaned(context: Context, atStart: StartSnapshot) {
        val registryDir = File(context.filesDir, REGISTRY_SUBDIR)
        val known = FerryEngine.batches.value.associateBy { it.id }
        val survivingPaths = mutableListOf<String>()
        for (entry in registryDir.listFiles().orEmpty()) {
            if (entry.name !in atStart.registryNames) {
                continue
            }
            val batch = known[entry.name]
            if (batch == null || batch.state == EngineTransferState.DONE) {
                deleteRegistered(entry)
            } else {
                survivingPaths += entry.readLines()
            }
        }
        deleteUnnamedCacheFolders(context, survivingPaths, atStart.cacheDirNames)
    }

    // Every per-share folder under the cache that no path in
    // survivingPaths sits inside is removed outright: nothing still
    // pending names it, so it is either already spent or was never
    // registered at all before the process that made it died.
    //
    // Only a folder in atStartCacheDirNames is ever looked at here, for
    // the same reason sweepOrphaned above skips a registry entry it did
    // not see at start: a folder made after the snapshot can still be
    // mid-copy, or already registered under a request id, and is not
    // this pass's to remove.
    private fun deleteUnnamedCacheFolders(
        context: Context,
        survivingPaths: List<String>,
        atStartCacheDirNames: Set<String>,
    ) {
        val namedDirs = survivingPaths.mapNotNull { File(it).parentFile?.absolutePath }.toSet()
        val cacheRoot = File(context.cacheDir, ShareIntake.CACHE_SUBDIR)
        for (dir in cacheRoot.listFiles().orEmpty()) {
            if (dir.name !in atStartCacheDirNames) {
                continue
            }
            if (dir.absolutePath !in namedDirs) {
                dir.deleteRecursively()
            }
        }
    }

    // The live half of the same rule: a batch that was still running at
    // start reaches Done later, observed from the batches flow FerryEngine
    // already exposes.
    fun reconcile(context: Context, batches: List<BatchInfo>) {
        val entries = File(context.filesDir, REGISTRY_SUBDIR).listFiles().orEmpty()
        if (entries.isEmpty()) {
            return
        }
        val done = batches.filter { it.state == EngineTransferState.DONE }.map { it.id }.toSet()
        for (entry in entries) {
            if (entry.name in done) {
                deleteRegistered(entry)
            }
        }
    }

    private fun deleteRegistered(registryFile: File) {
        registryFile.readLines().forEach { path ->
            val file = File(path)
            file.delete()
            file.parentFile?.delete()
        }
        registryFile.delete()
    }
}
