import Foundation
import ClowderCore

/// Runs the bundled `clowder` binary. The only `CommandRunner` in the app; everything that decides
/// *what* to run lives in ClowderCore's `HostRegistry`, which is unit-tested against a fake.
final class ProcessCommandRunner: CommandRunner, @unchecked Sendable {
    private let executablePath: String

    init(executablePath: String) {
        self.executablePath = executablePath
    }

    func run(_ args: [String], stdin: String?) throws -> CommandResult {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: executablePath)
        proc.arguments = args

        let out = Pipe()
        let err = Pipe()
        proc.standardOutput = out
        proc.standardError = err

        let input = Pipe()
        proc.standardInput = input

        try proc.run()

        // Write and close stdin BEFORE draining: the child reads to EOF on --token-stdin, so
        // leaving the pipe open would deadlock both sides.
        if let stdin {
            input.fileHandleForWriting.write(Data(stdin.utf8))
        }
        try? input.fileHandleForWriting.close()

        // Read BEFORE waiting: draining after waitUntilExit() can deadlock if the child fills the
        // pipe buffer. Output is small today, but read-before-wait is the safe order — the same
        // discipline the removed `resolveRemoteHost` documented.
        let stdoutData = out.fileHandleForReading.readDataToEndOfFile()
        let stderrData = err.fileHandleForReading.readDataToEndOfFile()
        proc.waitUntilExit()

        return CommandResult(status: proc.terminationStatus, stdout: stdoutData, stderr: stderrData)
    }
}
