// Chain script - demonstrates multiple host.call() invocations

export default function(input) {
    // First call - get handler info
    var info = host.call("test", {
        method: "GET",
        path: "/"
    });

    // Second call - different endpoint
    var second = host.call("test", {
        method: "GET",
        path: "/test/storage"
    });

    // Return aggregated results
    return {
        script: "chain",
        calls: 2,
        first: {
            status: info.status,
            service: info.body.service || "unknown"
        },
        second: {
            status: second.status
        },
        input: input
    };
}
