# Aurora `.asset` CLI Packer (Source)

Clean command-line wrapper around `AuroraAsset.dll` from [XboxUnity/AuroraAssetEditor](https://github.com/XboxUnity/AuroraAssetEditor) for packing and verifying Aurora `.asset` (`RXEA`) containers directly on Windows without GUI overhead.

## Build (Windows x86)

1. Download `AuroraAsset.dll` (32-bit x86) from the official `XboxUnity/AuroraAssetEditor` release and place it next to the compiled executable.
2. Compile with .NET Framework (`csc.exe`):

```powershell
C:\Windows\Microsoft.NET\Framework\v4.0.30319\csc.exe /platform:x86 /optimize+ /reference:System.Drawing.dll /out:aurora_asset_packer.exe Program.cs
```

## Usage

```text
aurora_asset_packer.exe pack <out.asset> <slot=image_path> [...]
aurora_asset_packer.exe verify <in.asset>
```

Valid slots: `icon` (0), `banner` (1), `boxart` (2), `background` (4), `ss1`..`ss20` (5..24).
