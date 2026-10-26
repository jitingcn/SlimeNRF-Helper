if (-not ('SlimeNRF.FilePublisher' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using Microsoft.Win32.SafeHandles;

namespace SlimeNRF
{
    public static class FilePublisher
    {
        private const uint Read = 0x80000000;
        private const uint ReadAttributes = 0x80;
        private const uint ShareRead = 1;
        private const uint ShareWrite = 2;
        private const uint ShareDelete = 4;
        private const uint OpenExisting = 3;
        private const uint OpenReparsePoint = 0x00200000;
        private const uint BackupSemantics = 0x02000000;
        private const uint DirectoryAttribute = 0x10;
        private const uint ReparseAttribute = 0x400;

        [StructLayout(LayoutKind.Sequential)]
        private struct FileInformation
        {
            public uint Attributes;
            public System.Runtime.InteropServices.ComTypes.FILETIME CreationTime;
            public System.Runtime.InteropServices.ComTypes.FILETIME AccessTime;
            public System.Runtime.InteropServices.ComTypes.FILETIME WriteTime;
            public uint Volume;
            public uint SizeHigh;
            public uint SizeLow;
            public uint Links;
            public uint IndexHigh;
            public uint IndexLow;
        }

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern SafeFileHandle CreateFileW(string path, uint access, uint sharing,
            IntPtr security, uint creation, uint flags, IntPtr template);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool GetFileInformationByHandle(SafeFileHandle file, out FileInformation info);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CreateDirectoryW(string path, IntPtr security);

        private static SafeFileHandle Open(string path, bool directory, bool allowDelete)
        {
            uint sharing = ShareRead | (directory ? ShareWrite : 0) | (allowDelete ? ShareDelete : 0);
            SafeFileHandle handle = CreateFileW(path, directory ? ReadAttributes : Read, sharing,
                IntPtr.Zero, OpenExisting, OpenReparsePoint | (directory ? BackupSemantics : 0), IntPtr.Zero);
            if (handle.IsInvalid)
            {
                int error = Marshal.GetLastWin32Error();
                handle.Dispose();
                if (error == 2) throw new FileNotFoundException("Publication file is missing.", path);
                throw new IOException("Cannot lock publication path: " + path, new Win32Exception(error));
            }
            try
            {
                FileInformation info = Information(handle);
                if ((info.Attributes & ReparseAttribute) != 0 ||
                    ((info.Attributes & DirectoryAttribute) != 0) != directory)
                    throw new IOException("Publication path must be regular and not a reparse point: " + path);
                return handle;
            }
            catch { handle.Dispose(); throw; }
        }

        private static FileInformation Information(SafeFileHandle handle)
        {
            FileInformation info;
            if (!GetFileInformationByHandle(handle, out info))
                throw new IOException("Cannot inspect locked publication file.", new Win32Exception(Marshal.GetLastWin32Error()));
            return info;
        }

        private static string FullPath(string path)
        {
            string full = Path.GetFullPath(path);
            string root = Path.GetPathRoot(full);
            if (full.StartsWith(@"\\?\", StringComparison.Ordinal) ||
                full.StartsWith(@"\\.\", StringComparison.Ordinal) ||
                full.Substring(root.Length).IndexOf(':') >= 0 ||
                (root.StartsWith(@"\\", StringComparison.Ordinal) &&
                 root.TrimEnd('\\').EndsWith(@"\pipe", StringComparison.OrdinalIgnoreCase)))
                throw new IOException("Publication paths must be ordinary filesystem paths.");
            foreach (string component in full.Substring(root.Length).Split('\\', '/'))
            {
                string name = component.Split('.')[0].TrimEnd(' ');
                if (name.Equals("CON", StringComparison.OrdinalIgnoreCase) ||
                    name.Equals("PRN", StringComparison.OrdinalIgnoreCase) ||
                    name.Equals("AUX", StringComparison.OrdinalIgnoreCase) ||
                    name.Equals("NUL", StringComparison.OrdinalIgnoreCase) ||
                    name.Equals("CONIN$", StringComparison.OrdinalIgnoreCase) ||
                    name.Equals("CONOUT$", StringComparison.OrdinalIgnoreCase) ||
                    (name.Length == 4 && "123456789\u00b9\u00b2\u00b3".IndexOf(name[3]) >= 0 &&
                     (name.StartsWith("COM", StringComparison.OrdinalIgnoreCase) ||
                      name.StartsWith("LPT", StringComparison.OrdinalIgnoreCase))))
                    throw new IOException("Publication paths must not name devices.");
            }
            return full;
        }

        private sealed class DirectoryPins : IDisposable
        {
            private readonly List<SafeFileHandle> handles = new List<SafeFileHandle>();

            public DirectoryPins(string directory)
            {
                try
                {
                    Stack<string> parents = new Stack<string>();
                    for (DirectoryInfo item = new DirectoryInfo(directory); item != null; item = item.Parent)
                        parents.Push(item.FullName);
                    // Denying directory deletion keeps validated parents from being renamed underneath us.
                    while (parents.Count != 0) handles.Add(Open(parents.Pop(), true, false));
                }
                catch { Dispose(); throw; }
            }

            public void Dispose()
            {
                for (int index = handles.Count - 1; index >= 0; --index) handles[index].Dispose();
                handles.Clear();
            }
        }

        public sealed class Input : IDisposable
        {
            private readonly DirectoryPins parents;
            public string PathName { get; private set; }
            public FileStream Stream { get; private set; }
            public string Hash { get; private set; }
            public long Length { get { return Stream.Length; } }

            internal Input(string path, bool allowDelete)
            {
                PathName = FullPath(path);
                parents = new DirectoryPins(Path.GetDirectoryName(PathName));
                SafeFileHandle handle = null;
                try
                {
                    handle = Open(PathName, false, allowDelete);
                    Stream = new FileStream(handle, FileAccess.Read, 1024 * 1024, false);
                    handle = null;
                    using (SHA256 hash = SHA256.Create())
                        Hash = BitConverter.ToString(hash.ComputeHash(Stream)).Replace("-", "").ToLowerInvariant();
                    Stream.Position = 0;
                }
                catch
                {
                    if (handle != null) handle.Dispose();
                    Dispose();
                    throw;
                }
            }

            internal bool SameFile(SafeFileHandle other)
            {
                FileInformation left = Information(Stream.SafeFileHandle);
                FileInformation right = Information(other);
                return left.Volume == right.Volume && left.IndexHigh == right.IndexHigh && left.IndexLow == right.IndexLow;
            }

            public void Dispose()
            {
                if (Stream != null) Stream.Dispose();
                parents.Dispose();
            }
        }

        public sealed class Result
        {
            public string State { get; internal set; }
            public string Hash { get; internal set; }
            public long Length { get; internal set; }
        }

        public static Input OpenInput(string path) { return new Input(path, false); }

        public static string CreateWorkDirectory(string directory)
        {
            string parent = FullPath(directory);
            using (DirectoryPins pins = new DirectoryPins(parent))
            {
                string path = Path.Combine(parent, ".slimenrf-upx-" + Guid.NewGuid().ToString("N"));
                if (!CreateDirectoryW(path, IntPtr.Zero))
                    throw new IOException("Cannot create private UPX staging directory.", new Win32Exception(Marshal.GetLastWin32Error()));
                return path;
            }
        }

        public static Result Publish(string input, string destination, bool force)
        {
            string output = FullPath(destination);
            using (Input source = OpenInput(input))
            using (DirectoryPins pins = new DirectoryPins(Path.GetDirectoryName(output)))
            {
                Input existing = null;
                string temporary = null;
                try
                {
                    try { existing = new Input(output, true); }
                    catch (FileNotFoundException) { }
                    Result result = new Result { Hash = source.Hash, Length = source.Length };
                    if (existing != null)
                    {
                        if (existing.Hash == source.Hash)
                        {
                            result.State = "Identical";
                            return result;
                        }
                        if (!force)
                            throw new IOException("Refusing to overwrite a different existing package: " + output + " (use -Force to replace it)");
                    }

                    string staged = Path.Combine(Path.GetDirectoryName(output), ".slimenrf-publish-" + Guid.NewGuid().ToString("N") + ".tmp");
                    using (FileStream stage = new FileStream(staged, FileMode.CreateNew, FileAccess.Write, FileShare.None))
                    {
                        temporary = staged;
                        source.Stream.CopyTo(stage);
                        stage.Flush(true);
                    }
                    if (existing == null)
                    {
                        // Move refuses a destination created after the initial check, even with force.
                        File.Move(temporary, output);
                        result.State = "Created";
                    }
                    else
                    {
                        using (SafeFileHandle current = Open(output, false, true))
                        {
                            if (!existing.SameFile(current))
                                throw new IOException("Package destination changed while publishing: " + output);
                            // C# supplies a true null backup name, including on Windows PowerShell 5.1.
                            File.Replace(temporary, output, null);
                        }
                        result.State = "Replaced";
                    }
                    temporary = null;
                    return result;
                }
                finally
                {
                    if (existing != null) existing.Dispose();
                    if (temporary != null) File.Delete(temporary);
                }
            }
        }
    }
}
'@
}
