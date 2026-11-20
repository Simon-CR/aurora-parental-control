using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Runtime.InteropServices;

namespace AuroraAssetPacker
{
    internal static class Native
    {
        [DllImport("AuroraAsset.dll", CallingConvention = CallingConvention.Cdecl)]
        internal static extern int ConvertImageToAsset(
            IntPtr imageData,
            int imageDataLen,
            int imageWidth,
            int imageHeight,
            int useCompression,
            IntPtr headerData,
            out int headerDataLen,
            IntPtr videoData,
            out int videoDataLen);

        [DllImport("AuroraAsset.dll", CallingConvention = CallingConvention.Cdecl)]
        internal static extern int ConvertAssetToImage(
            IntPtr headerData,
            int headerDataLen,
            IntPtr videoData,
            int videoDataLen,
            IntPtr imageData,
            out int imageDataLen,
            out int imageWidth,
            out int imageHeight);
    }

    internal sealed class Entry
    {
        public uint Offset;
        public byte[] TextureHeader = new byte[52];
        public byte[] VideoData = new byte[0];
    }

    internal static class Program
    {
        private const uint MagicRxea = 0x52584541; // "RXEA" BE
        private const int MaxSlots = 25;         // 0=icon,1=banner,2=boxart,3=slot,4=background,5..24=ss1..20
        private const int HeaderAndTableBytes = 20 + (MaxSlots * 64); // 1620
        private const int DataOffset = 2048;       // aligned to 0x800 sector

        private static int Main(string[] args)
        {
            if (args.Length < 2)
            {
                Console.Error.WriteLine("Usage:");
                Console.Error.WriteLine("  aurora_asset_packer.exe pack <out.asset> <slot=image_path> [...]");
                Console.Error.WriteLine("  aurora_asset_packer.exe verify <in.asset>");
                return 2;
            }

            string cmd = args[0].ToLowerInvariant();
            try
            {
                if (cmd == "pack")
                {
                    if (args.Length < 3)
                    {
                        Console.Error.WriteLine("pack requires <out.asset> and at least one <slot=image_path>");
                        return 2;
                    }
                    return CmdPack(args);
                }
                if (cmd == "verify")
                {
                    return CmdVerify(args[1]);
                }
                Console.Error.WriteLine("Unknown command: " + args[0]);
                return 2;
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine("ERROR: " + ex.Message);
                return 1;
            }
        }

        private static int ParseSlot(string name)
        {
            switch (name.ToLowerInvariant())
            {
                case "icon": return 0;
                case "banner": return 1;
                case "boxart":
                case "cover": return 2;
                case "slot": return 3;
                case "background":
                case "bg": return 4;
                default:
                    if (name.StartsWith("ss", StringComparison.OrdinalIgnoreCase))
                    {
                        int num;
                        if (int.TryParse(name.Substring(2), out num) && num >= 1 && num <= 20)
                            return 4 + num;
                    }
                    throw new ArgumentException("Invalid asset slot: " + name);
            }
        }

        private static byte[] LoadImageAsArgbBytes(string path, out int width, out int height)
        {
            using (var raw = Image.FromFile(path))
            {
                width = raw.Width;
                height = raw.Height;
                using (var bmp = new Bitmap(width, height, PixelFormat.Format32bppArgb))
                {
                    using (var g = Graphics.FromImage(bmp))
                    {
                        g.DrawImage(raw, 0, 0, width, height);
                    }
                    var rect = new Rectangle(0, 0, width, height);
                    var data = bmp.LockBits(rect, ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
                    try
                    {
                        int stride = Math.Abs(data.Stride);
                        byte[] rowBuf = new byte[stride];
                        byte[] argb = new byte[width * height * 4];
                        int outIdx = 0;
                        for (int y = 0; y < height; y++)
                        {
                            IntPtr rowPtr = IntPtr.Add(data.Scan0, y * data.Stride);
                            Marshal.Copy(rowPtr, rowBuf, 0, stride);
                            for (int x = 0; x < width * 4; x += 4)
                            {
                                // Format32bppArgb stores [B, G, R, A]; AuroraAsset.dll expects [A, R, G, B].
                                argb[outIdx++] = rowBuf[x + 3];
                                argb[outIdx++] = rowBuf[x + 2];
                                argb[outIdx++] = rowBuf[x + 1];
                                argb[outIdx++] = rowBuf[x];
                            }
                        }
                        return argb;
                    }
                    finally
                    {
                        bmp.UnlockBits(data);
                    }
                }
            }
        }

        private static void EncodeImage(string path, out byte[] texHeader, out byte[] videoData)
        {
            int width, height;
            byte[] pixels = LoadImageAsArgbBytes(path, out width, out height);
            IntPtr pd = IntPtr.Zero, hd = IntPtr.Zero, vd = IntPtr.Zero;
            try
            {
                pd = Marshal.AllocHGlobal(pixels.Length);
                Marshal.Copy(pixels, 0, pd, pixels.Length);

                int hLen, vLen;
                if (Native.ConvertImageToAsset(pd, pixels.Length, width, height, 1, IntPtr.Zero, out hLen, IntPtr.Zero, out vLen) != 1)
                    throw new InvalidOperationException("ConvertImageToAsset size probe failed for " + path);
                if (hLen != 52 || vLen <= 0)
                    throw new InvalidOperationException("Unexpected ConvertImageToAsset lengths (" + hLen + ", " + vLen + ") for " + path);

                hd = Marshal.AllocHGlobal(hLen);
                vd = Marshal.AllocHGlobal(vLen);
                if (Native.ConvertImageToAsset(pd, pixels.Length, width, height, 1, hd, out hLen, vd, out vLen) != 1)
                    throw new InvalidOperationException("ConvertImageToAsset encode failed for " + path);

                texHeader = new byte[hLen];
                videoData = new byte[vLen];
                Marshal.Copy(hd, texHeader, 0, hLen);
                Marshal.Copy(vd, videoData, 0, vLen);
            }
            finally
            {
                if (pd != IntPtr.Zero) Marshal.FreeHGlobal(pd);
                if (hd != IntPtr.Zero) Marshal.FreeHGlobal(hd);
                if (vd != IntPtr.Zero) Marshal.FreeHGlobal(vd);
            }
        }

        private static int CmdPack(string[] args)
        {
            string outPath = args[1];
            var entries = new Entry[MaxSlots];
            for (int i = 0; i < MaxSlots; i++)
                entries[i] = new Entry();

            for (int i = 2; i < args.Length; i++)
            {
                int eq = args[i].IndexOf('=');
                if (eq <= 0 || eq >= args[i].Length - 1)
                    throw new ArgumentException("Expected <slot=image_path>, got: " + args[i]);
                int slot = ParseSlot(args[i].Substring(0, eq));
                string imgPath = args[i].Substring(eq + 1);

                byte[] header, video;
                EncodeImage(imgPath, out header, out video);
                entries[slot].TextureHeader = header;
                entries[slot].VideoData = video;
            }

            uint offset = 0;
            uint dataSize = 0;
            uint flags = 0;
            uint ssCount = 0;
            for (int i = 0; i < MaxSlots; i++)
            {
                if (entries[i].VideoData.Length == 0) continue;
                entries[i].Offset = offset;
                uint sz = (uint)entries[i].VideoData.Length;
                offset += sz;
                dataSize += sz;
                flags |= (1u << i);
                if (i >= 5 && i < MaxSlots)
                    ssCount++;
            }

            string dir = Path.GetDirectoryName(Path.GetFullPath(outPath));
            if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);

            using (var fs = File.Create(outPath))
            using (var bw = new BinaryWriter(fs))
            {
                WriteBE(bw, MagicRxea);
                WriteBE(bw, 1u);
                WriteBE(bw, dataSize);
                WriteBE(bw, flags);
                WriteBE(bw, ssCount);
                for (int i = 0; i < MaxSlots; i++)
                {
                    WriteBE(bw, entries[i].Offset);
                    WriteBE(bw, (uint)entries[i].VideoData.Length);
                    WriteBE(bw, 0u);
                    bw.Write(entries[i].TextureHeader, 0, 52);
                }
                bw.Write(new byte[DataOffset - HeaderAndTableBytes]);
                for (int i = 0; i < MaxSlots; i++)
                {
                    if (entries[i].VideoData.Length > 0)
                        bw.Write(entries[i].VideoData);
                }
            }

            Console.WriteLine("PACK OK: {0} ({1} payload bytes, flags=0x{2:X}, screenshots={3})", outPath, dataSize, flags, ssCount);
            return 0;
        }

        private static int CmdVerify(string inPath)
        {
            byte[] data = File.ReadAllBytes(inPath);
            if (data.Length < DataOffset)
                throw new InvalidDataException("Asset too short: " + data.Length);

            uint magic = ReadBE(data, 0);
            uint version = ReadBE(data, 4);
            uint dataSize = ReadBE(data, 8);
            uint flags = ReadBE(data, 12);
            uint ssCount = ReadBE(data, 16);

            if (magic != MagicRxea || version != 1)
                throw new InvalidDataException(string.Format("Invalid header magic=0x{0:X} version={1}", magic, version));

            int decoded = 0;
            int cursor = 20;
            for (int i = 0; i < MaxSlots; i++, cursor += 64)
            {
                uint offset = ReadBE(data, cursor);
                uint size = ReadBE(data, cursor + 4);
                if (size == 0) continue;

                int start = DataOffset + (int)offset;
                if (start + (int)size > data.Length)
                    throw new InvalidDataException("Entry " + i + " out of bounds");

                byte[] hdr = new byte[52];
                byte[] vid = new byte[size];
                Buffer.BlockCopy(data, cursor + 12, hdr, 0, 52);
                Buffer.BlockCopy(data, start, vid, 0, (int)size);

                int w, h;
                if (!VerifyDecode(hdr, vid, out w, out h))
                    throw new InvalidDataException("Failed decoding slot " + i);
                decoded++;
                Console.WriteLine("  slot {0}: {1}x{2} ({3} bytes)", i, w, h, size);
            }

            Console.WriteLine("VERIFY OK: {0} (dataSize={1}, flags=0x{2:X}, ssCount={3}, decoded={4})",
                inPath, dataSize, flags, ssCount, decoded);
            return 0;
        }

        private static bool VerifyDecode(byte[] hdr, byte[] vid, out int w, out int h)
        {
            IntPtr hd = IntPtr.Zero, vd = IntPtr.Zero, pd = IntPtr.Zero;
            w = 0;
            h = 0;
            try
            {
                hd = Marshal.AllocHGlobal(hdr.Length);
                Marshal.Copy(hdr, 0, hd, hdr.Length);
                vd = Marshal.AllocHGlobal(vid.Length);
                Marshal.Copy(vid, 0, vd, vid.Length);

                int imgLen;
                if (Native.ConvertAssetToImage(hd, hdr.Length, vd, vid.Length, IntPtr.Zero, out imgLen, out w, out h) != 1)
                    return false;
                pd = Marshal.AllocHGlobal(imgLen);
                return Native.ConvertAssetToImage(hd, hdr.Length, vd, vid.Length, pd, out imgLen, out w, out h) == 1 && w > 0 && h > 0;
            }
            finally
            {
                if (pd != IntPtr.Zero) Marshal.FreeHGlobal(pd);
                if (hd != IntPtr.Zero) Marshal.FreeHGlobal(hd);
                if (vd != IntPtr.Zero) Marshal.FreeHGlobal(vd);
            }
        }

        private static void WriteBE(BinaryWriter bw, uint val)
        {
            bw.Write((byte)((val >> 24) & 0xFF));
            bw.Write((byte)((val >> 16) & 0xFF));
            bw.Write((byte)((val >> 8) & 0xFF));
            bw.Write((byte)(val & 0xFF));
        }

        private static uint ReadBE(byte[] b, int off)
        {
            return ((uint)b[off] << 24) | ((uint)b[off + 1] << 16) | ((uint)b[off + 2] << 8) | b[off + 3];
        }
    }
}
