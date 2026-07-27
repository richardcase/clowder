import Foundation

/// A ControlTransport over a POSIX Unix-domain stream socket. The read loop runs on a
/// background queue and delivers each complete line ON THE MAIN QUEUE, so downstream
/// AgentStore @Published mutations are main-thread-safe.
public final class UnixSocketConnection: ControlTransport {
    private let fd: Int32
    private var receiver: ((String) -> Void)?
    private let readQueue = DispatchQueue(label: "muxy.control.read")
    private var isRunning = true
    private var isClosed = false

    public init(path: String) throws {
        fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let maxLen = MemoryLayout.size(ofValue: addr.sun_path) // 104 on macOS
        let pathBytes = path.utf8CString                       // includes NUL
        guard pathBytes.count <= maxLen else {
            close(fd)
            throw POSIXError(.ENAMETOOLONG)
        }
        withUnsafeMutablePointer(to: &addr.sun_path) { p in
            p.withMemoryRebound(to: CChar.self, capacity: maxLen) { dst in
                for (i, b) in pathBytes.enumerated() where i < maxLen { dst[i] = b }
            }
        }
        let len = socklen_t(MemoryLayout<sockaddr_un>.size)
        let rc = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, len) }
        }
        guard rc == 0 else {
            let err = errno
            close(fd)
            throw POSIXError(POSIXErrorCode(rawValue: err) ?? .ECONNREFUSED)
        }
    }

    public func setReceiver(_ receiver: @escaping (String) -> Void) {
        self.receiver = receiver
        readQueue.async { [weak self] in self?.readLoop() }
    }

    private func readLoop() {
        var buf = [UInt8](repeating: 0, count: 4096)
        var lineBuffer = LineBuffer()
        while isRunning {
            let n = read(fd, &buf, buf.count)
            if n <= 0 { break }
            let lines = lineBuffer.append(Data(buf[0..<n]))
            for line in lines {
                DispatchQueue.main.async { [weak self] in self?.receiver?(line) }
            }
        }
    }

    /// Proactively close the connection: unblocks the read loop and closes the fd.
    /// Idempotent; also called from deinit.
    public func disconnect() {
        guard !isClosed else { return }
        isClosed = true
        isRunning = false
        shutdown(fd, SHUT_RDWR)   // unblocks a blocked read() so readLoop() can exit
        close(fd)
    }

    public func send(line: String) throws {
        // Never touch `fd` once disconnected: the OS may have already reused that
        // integer for an unrelated descriptor elsewhere in the process, and writing
        // to it could hit a broken pipe on someone else's socket (SIGPIPE, fatal by
        // default) instead of failing cleanly here.
        guard !isClosed else { throw POSIXError(.EBADF) }
        let bytes = Array((line + "\n").utf8)
        try bytes.withUnsafeBytes { raw in
            var off = 0
            while off < raw.count {
                let n = write(fd, raw.baseAddress!.advanced(by: off), raw.count - off)
                if n <= 0 { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
                off += n
            }
        }
    }

    deinit { disconnect() }
}
