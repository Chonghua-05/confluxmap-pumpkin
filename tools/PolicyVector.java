import cn.net.rms.confluxmap.core.net.HelloPolicyS2C;
import cn.net.rms.confluxmap.core.net.Message;
import cn.net.rms.confluxmap.core.net.MsgCodec;
import cn.net.rms.confluxmap.core.net.Proto;
import cn.net.rms.confluxmap.core.predict.WorldPreset;

import java.util.List;

/**
 * Cross-implementation test vector generator for the confluxmap wire protocol.
 *
 * <p>Compiles against the real confluxmap {@code common} module (which is
 * deliberately Minecraft-free), so the bytes it prints are produced by the
 * same encoder the Paper companion ships. The Rust port asserts its own
 * encoder against those exact bytes, and the plugin's live traffic can be fed
 * back through {@code decode} to prove a real client would accept it.
 *
 * <p>Usage:
 * <pre>
 *   java -cp &lt;classes&gt;:&lt;common-classes&gt; PolicyVector            # print golden HEX
 *   java -cp ... PolicyVector decode &lt;hex&gt;                   # decode and dump fields
 * </pre>
 *
 * <p>Build (from the confluxmap repo root):
 * <pre>
 *   javac -d build/cfm-classes -sourcepath common/src/main/java \
 *         common/src/main/java/cn/net/rms/confluxmap/core/net/MsgCodec.java
 *   javac -cp build/cfm-classes -d build/cfm-classes tools/PolicyVector.java
 * </pre>
 */
public final class PolicyVector {

    /**
     * A deliberately synthetic seed. A real world's seed must never be baked into
     * a public test vector, and nothing here depends on the value being real: the
     * vector only pins the encoding layout.
     */
    static final long SEED = 0x0123_4567_89AB_CDEFL;

    /** Derived exactly as the plugin derives it: low 48 bits of the seed. */
    static final String WORLD_ID = "00000000-0000-0000-0000-456789abcdef";

    /** The Pumpkin server's Minecraft version, which is what the client feeds to cubiomes. */
    static final String WORLDGEN = "26.2";

    private PolicyVector() {
    }

    public static void main(final String[] args) throws Exception {
        if (args.length >= 2 && "decode".equals(args[0])) {
            decode(args[1]);
            return;
        }
        emit();
    }

    /** Prints the reference encoding of the seed-only policy this plugin sends. */
    private static void emit() throws Exception {
        final HelloPolicyS2C policy = new HelloPolicyS2C(
            // seedGranted = true, correctionsEnabled = false, everything else off.
            new HelloPolicyS2C.Flags(true, false, false),
            WORLD_ID,
            WORLDGEN,
            new HelloPolicyS2C.Budgets(
                Proto.DEFAULT_MAX_BYTES_PER_SEC,
                Proto.DEFAULT_MAX_TILES_PER_REQ,
                Proto.DEFAULT_MIN_REQ_INTERVAL_MS,
                Proto.DEFAULT_MAX_PATCH_LOD),
            List.of(new HelloPolicyS2C.DimDescriptor(
                "minecraft:overworld", "overworld", true, true, SEED, WorldPreset.DEFAULT)));

        final byte[] encoded = MsgCodec.encode(policy);
        System.out.println("len=" + encoded.length);
        System.out.println("hex=" + toHex(encoded));
    }

    /** Decodes a hex payload with the reference decoder, proving a client would accept it. */
    private static void decode(final String hex) throws Exception {
        final byte[] payload = fromHex(hex.replace(" ", ""));
        final Message message = MsgCodec.decode(payload);
        if (!(message instanceof HelloPolicyS2C)) {
            System.out.println("DECODE_FAIL unexpected type: " + message.getClass().getName());
            System.exit(1);
            return;
        }
        final HelloPolicyS2C policy = (HelloPolicyS2C) message;
        final HelloPolicyS2C.DimDescriptor dim = policy.dims().get(0);
        System.out.println("DECODE_OK"
            + " type=" + message.getClass().getSimpleName()
            + " len=" + payload.length
            + " seedGranted=" + policy.flags().seedGranted()
            + " correctionsEnabled=" + policy.flags().correctionsEnabled()
            + " worldId=" + policy.worldId()
            + " worldgenVersion=" + policy.worldgenVersion()
            + " maxBytesPerSec=" + policy.budgets().maxBytesPerSec()
            + " maxTilesPerReq=" + policy.budgets().maxTilesPerReq()
            + " minReqIntervalMs=" + policy.budgets().minReqIntervalMs()
            + " maxPatchLod=" + policy.budgets().maxPatchLod()
            + " dimCount=" + policy.dims().size()
            + " dim0=" + dim.dimId() + "/" + dim.dimType()
            + " predictable=" + dim.predictable()
            + " hasSeed=" + dim.hasSeed()
            + " preset=" + dim.preset()
            + " seed=" + dim.seed());

        if (dim.seed() != SEED) {
            System.out.println("DECODE_FAIL seed mismatch: " + dim.seed() + " != " + SEED);
            System.exit(1);
        }
    }

    private static String toHex(final byte[] bytes) {
        final StringBuilder sb = new StringBuilder(bytes.length * 2);
        for (final byte b : bytes) {
            sb.append(Character.forDigit((b >> 4) & 0xF, 16));
            sb.append(Character.forDigit(b & 0xF, 16));
        }
        return sb.toString();
    }

    private static byte[] fromHex(final String hex) {
        final byte[] out = new byte[hex.length() / 2];
        for (int i = 0; i < out.length; i++) {
            out[i] = (byte) Integer.parseInt(hex.substring(i * 2, i * 2 + 2), 16);
        }
        return out;
    }
}
