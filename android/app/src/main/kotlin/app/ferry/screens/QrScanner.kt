package app.ferry.screens

import android.util.Size
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.viewinterop.AndroidView
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage
import java.util.concurrent.Executors

// The camera, looking for the Mac's pairing code.
//
// A view, not a component: one screen, one platform, one state. The camera
// preview and the barcode reader are both the platform's own, so there is
// nothing custom to build and nothing drawn by hand.
//
// It reports raw bytes and parses nothing. The payload is the engine's to
// read: offerScanned takes it whole, and every check — the version, the
// expiry, the nonce, whether it is a Ferry code at all — happens on that
// side, where the four error codes for those answers already live.
//
// One scan only. onScanned is called once and then this view stops
// reading, because a QR code stays in frame for many frames and a second
// call would race the first through the handshake.
@Composable
fun QrScanner(
    onScanned: (ByteArray) -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current

    // The reader and its thread outlive a recomposition, so they are
    // remembered rather than rebuilt. The executor is shut down when this
    // view leaves.
    val executor = remember { Executors.newSingleThreadExecutor() }
    val scanner = remember { BarcodeScanning.getClient() }
    val previewView = remember { PreviewView(context) }
    // Guards the one-scan rule above. Read and written only on the
    // executor's thread.
    val sent = remember { booleanArrayOf(false) }

    DisposableEffect(lifecycleOwner) {
        val providerFuture = ProcessCameraProvider.getInstance(context)
        val provider = providerFuture.get()

        val preview = Preview.Builder().build().also {
            it.surfaceProvider = previewView.surfaceProvider
        }

        // 1280 by 720 is enough to read a code from across a desk and
        // cheap enough that the reader keeps up on a Pixel 3 XL. A larger
        // frame costs time and finds nothing more.
        val analysis = ImageAnalysis.Builder()
            .setResolutionSelector(
                ResolutionSelector.Builder()
                    .setResolutionStrategy(
                        ResolutionStrategy(
                            Size(ANALYSIS_WIDTH, ANALYSIS_HEIGHT),
                            ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER,
                        ),
                    )
                    .build(),
            )
            // The newest frame, not a queue of them. A stale frame is of no
            // use: the code is either in front of the camera now or it is
            // not.
            .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
            .build()

        analysis.setAnalyzer(executor) { imageProxy ->
            val image = imageProxy.image
            if (image == null || sent[0]) {
                imageProxy.close()
                return@setAnalyzer
            }
            val input = InputImage.fromMediaImage(
                image,
                imageProxy.imageInfo.rotationDegrees,
            )
            scanner.process(input)
                .addOnSuccessListener { barcodes ->
                    if (sent[0]) {
                        return@addOnSuccessListener
                    }
                    // The Mac's offer is ASCII: "FERRY1:" then base64url.
                    // Anything else is somebody else's QR code, and the
                    // engine says so in words rather than this view
                    // guessing.
                    val payload = barcodes
                        .firstOrNull { it.format == Barcode.FORMAT_QR_CODE }
                        ?.rawValue
                    if (payload != null) {
                        sent[0] = true
                        onScanned(payload.toByteArray())
                    }
                }
                .addOnCompleteListener { imageProxy.close() }
        }

        provider.unbindAll()
        provider.bindToLifecycle(
            lifecycleOwner,
            CameraSelector.DEFAULT_BACK_CAMERA,
            preview,
            analysis,
        )

        onDispose {
            provider.unbindAll()
            analysis.clearAnalyzer()
            scanner.close()
            executor.shutdown()
        }
    }

    AndroidView(factory = { previewView }, modifier = modifier)
}

private const val ANALYSIS_WIDTH = 1_280
private const val ANALYSIS_HEIGHT = 720
