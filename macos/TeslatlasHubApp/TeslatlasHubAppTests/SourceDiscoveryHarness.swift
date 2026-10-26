// SPDX-License-Identifier: AGPL-3.0-only

import Foundation
#if canImport(Teslatlas_Hub)
@testable import Teslatlas_Hub
#endif

/// Runs the production discovery script through the production bounded executor.
/// Call from a worker thread; the process completion runs on the executor queue.
enum SourceDiscoveryHarness {
    static func execute(environment: [String: String]) -> Result<String, Error> {
        let done = DispatchSemaphore(value: 0)
        let lock = NSLock()
        var outcome: Result<String, Error>?
        let completionQuiescence = HubChildQuiescence()
        completionQuiescence.childMayRun()
        HubProcessExecutor.run(executable: URL(fileURLWithPath: "/bin/sh"),
            arguments: ["-s"], stdin: SourceServerImporter.discoveryScriptForTests,
            environment: environment, maximumOutputBytes: 64 * 1024, timeout: 30) { result in
                if case let .failure(HubActionError.commandNotQuiescent(_, pending)) = result, !pending.isQuiescent {
                    pending.whenQuiescent { completionQuiescence.childReaped() }
                } else { completionQuiescence.childReaped() }
                lock.lock(); outcome = result; lock.unlock(); done.signal()
            }
        guard done.wait(timeout: .now() + 35) == .success else {
            return .failure(completionQuiescence.isQuiescent ? HubActionError.commandTimedOut
                : HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, completionQuiescence))
        }
        lock.lock(); defer { lock.unlock() }
        return outcome!
    }

    struct Observation {
        let status: Int32
        let output: String
        let calls: [[String]]
        let values: [String: String]?
        let classification: SourceVersionClassification?
        let versionAdmitted: Bool
        let acknowledgementRequired: Bool
    }

    static func run(scenario: [String: Any], useSudo: Bool = false) throws -> Observation {
        let root = HubTestScratch.fixtureRoot("source-discovery")
        let bin = root.appendingPathComponent("bin")
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700])
        var retainUntilQuiescent = false
        defer { if !retainUntilQuiescent { try? FileManager.default.removeItem(at: root) } }
        let temporary = root.appendingPathComponent("tmp")
        try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700])
        let scene = root.appendingPathComponent("scenario.json")
        let calls = root.appendingPathComponent("calls.jsonl")
        try JSONSerialization.data(withJSONObject: scenario).write(to: scene)
        try Data().write(to: calls)
        for name in ["docker", "sudo", "id"] {
            let command = bin.appendingPathComponent(name)
            try Data(fakeCommand.utf8).write(to: command)
            try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: command.path)
        }
        let result = execute(environment: [
            "PATH": bin.path + ":" + (ProcessInfo.processInfo.environment["PATH"] ?? "/usr/bin:/bin"),
            "SCENARIO": scene.path, "CALLS": calls.path, "TMPDIR": temporary.path,
            "TESLATLAS_USE_SUDO": useSudo ? "1" : "0"
        ])
        let status: Int32, output: String
        switch result {
        case let .success(text): status = 0; output = text
        case let .failure(HubActionError.commandExited(code, text)): status = code; output = text
        case let .failure(error):
            if case let HubActionError.commandNotQuiescent(_, pending) = error, !pending.isQuiescent {
                retainUntilQuiescent = true
                pending.whenQuiescent { try? FileManager.default.removeItem(at: root) }
            }
            throw error
        }
        let log = try String(contentsOf: calls, encoding: .utf8).split(whereSeparator: \.isNewline).map {
            try JSONSerialization.jsonObject(with: Data($0.utf8)) as! [String]
        }
        let values = try? SourceServerImporter.parseForTests(output)
        let classification = values.map { SourceServerImporter.classifyVersionForTests(ociVersion: $0["oci_version"]) }
        let admitted: Bool, acknowledgement: Bool
        if let values {
            do { acknowledgement = try SourceDiscovery.admittedVersion(ociVersion: values["oci_version"]) == nil; admitted = true }
            catch { acknowledgement = false; admitted = false }
        } else { admitted = false; acknowledgement = false }
        return Observation(status: status, output: output, calls: log, values: values,
            classification: classification, versionAdmitted: admitted, acknowledgementRequired: acknowledgement)
    }

    static let fakeCommand = #"""
#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
s=json.loads(Path(os.environ['SCENARIO']).read_text());name=Path(sys.argv[0]).name;a=sys.argv[1:]
with Path(os.environ['CALLS']).open('a') as f:f.write(json.dumps([name]+a)+'\n')
def out(value):print(value);raise SystemExit(0)
def reject():print('Unexpected fake command',file=sys.stderr);raise SystemExit(99)
if Path(os.environ.get('TMPDIR','')).resolve()!=(Path(os.environ['SCENARIO']).parent/'tmp').resolve():reject()
if name=='id':
 if a==['-u']:out(str(s.get('uid',501)))
 reject()
if name=='sudo':
 if a[:2]!=['-n','docker']:reject()
 a=a[2:]
elif name!='docker':reject()
if a and a[0]=='ps':
 if a==['ps','--filter','label=com.docker.compose.service=database','--format','{{.ID}}']:out('\n'.join(s['databases']))
 if a[-2:]!=['--format','{{.ID}}']:reject()
 filters=[a[i+1] for i,v in enumerate(a[:-1]) if v=='--filter']
 project=next((f.split('=',2)[2] for f in filters if f.startswith('label=com.docker.compose.project=')),None)
 if project is None:reject()
 if filters==['label=com.docker.compose.project='+project]:out('\n'.join(sorted((k for k,v in s['containers'].items() if v['project']==project),key=lambda k:(k=='chosen-last',k))))
 if filters==['label=com.docker.compose.project='+project,'label=com.docker.compose.service=database']:out('\n'.join(k for k in s['databases'] if s['containers'][k]['project']==project))
 reject()
if len(a)==4 and a[:2]==['inspect','-f']:
 template,cid=a[2:]
 if cid not in s['containers']:reject()
 c=s['containers'][cid]
 if template=='{{index .Config.Labels "com.docker.compose.project"}}':out(c['project'])
 if template=='{{range .Config.Env}}{{index (split . "=") 0}}{{println}}{{end}}':out('\n'.join(c['env']))
 if template=='{{range .Config.Env}}{{println .}}{{end}}':out('\n'.join(k+'='+v for k,v in c['env'].items()))
 if template=='{{index .Config.Labels "org.opencontainers.image.version"}}':out(c.get('version',''))
 if template=='{{range $name, $value := .NetworkSettings.Networks}}{{println $name}}{{end}}' and cid in s['databases']:out('fictional-net')
 if template=='{{with index .NetworkSettings.Networks "fictional-net"}}{{.IPAddress}}{{end}}' and cid in s['databases']:out('172.20.0.9')
 reject()
if len(a)==9 and a[:1]==['exec'] and a[1] in s['databases'] and a[2:4]==['psql','-U'] and a[5]=='-d' and a[7:]==['-Atc','select id from cars order by id limit 2']:
 out('42')
reject()
"""#
}
