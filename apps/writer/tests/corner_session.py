#!/usr/bin/env python3
"""Isolated named-session fixture for the native Writer corner recovery test."""
import fcntl
import json
import os
import sys
from pathlib import Path

root = Path(os.environ["XDG_CONFIG_HOME"])
root.mkdir(parents=True, exist_ok=True)
with (root / "job-log").open("a+") as journal:
    fcntl.flock(journal, fcntl.LOCK_EX)
    journal.seek(0)
    jobs = [json.loads(line) for line in journal]
    session = "corner-test"
    position = list(jobs[-1]["position"]) if jobs else [0, 0, 0]
    base = dict(connected=True, device_id="sim-unit", controller="BtWriter",
                firmware="1.3a.20221114", port="virtual-corner", baud=115200,
                reference_generation=5, state="Idle", units="mm", work_offset=[0,0,0])
    def append(ops):
        results = []
        for idx, name in enumerate(ops):
            if name in ("plotter.jog", "plotter.jog_held"):
                axis = idx % 2 if len(ops)>1 else 0
                old = position[axis]
                position[axis] += 2
                data = dict(base, axis="XY"[axis], from_mm=old,
                            target_mm=position[axis], feed_mm_min=300)
                if name=="plotter.jog":
                    data = {k:data[k] for k in ("connected","port","baud","reference_generation",
                                                "state","axis","from_mm","target_mm","feed_mm_min")}
                else:
                    data["work_position"] = list(position)
            elif name=="plotter.status":
                data = dict(base,work_position=list(position))
            else:
                data = {}
            results.append(dict(step=idx,operation=name,value=data))
        job = dict(position=position,result=dict(status="completed",completed=len(ops),results=results))
        journal.write(json.dumps(job)+"\n")
        journal.flush()
        return len(jobs)+1

    if sys.argv[1]=="inject":
        append(sys.argv[2:])
        sys.exit(0)
    op=sys.argv[2]
    assert sys.argv[3] == session
    count=len(jobs)
    out=dict(ok=True,session=session,state_revision=2*count)
    if op=="status":
        out.update(status="ready",pid=int(os.environ["ZUSE_CORNER_DAEMON_PID"]),active_job_id=None,last_job_id=count or None)
    elif op=="result":
        idx=int(sys.argv[4])
        assert 1 <= idx <= count
        out.update(job_id=idx,status="completed",result_truncated=False,result=jobs[idx-1]["result"])
    else:
        assert op=="observe", "Writer attempted a motion or other modifying command"
        text=sys.stdin.read()
        assert text.startswith("plotter.status("),text
        rev=int(sys.argv[sys.argv.index("--expect-revision")+1])
        if rev!=2*count:
            out.update(ok=False,error={"message":"stale revision"})
        else:
            job=append(["plotter.status"])
            out.update(job_id=job,status="accepted",state_revision=2*count+1,
                       request_id=sys.argv[sys.argv.index("--request-id")+1])
    print(json.dumps(out))
