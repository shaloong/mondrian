// SPDX-License-Identifier: MIT
// Deterministic fault tests of the production native owner mechanics. These
// test owners do not implement a physical provider and never qualify hardware.
#include "bridge.cpp"
#include <iostream>
namespace {
void check(bool condition, const char* message) { if (!condition) throw std::runtime_error(message); }
struct ProbeState {
    std::mutex mutex; std::condition_variable wake;
    bool allow_initialize = true, allow_stop = true;
    std::atomic<bool> stop_entered{false};
    std::atomic<uint32_t> destroyed{0}, pumped{0};
};
struct ProbeOwner final : Owner {
    std::shared_ptr<ProbeState> proof;
    bool refuse_close = false;
    explicit ProbeOwner(std::shared_ptr<ProbeState> value) : Owner(0, 0, 0, 8), proof(std::move(value)) {}
    ~ProbeOwner() override { ++proof->destroyed; }
    void initialize() override {
        std::unique_lock<std::mutex> guard(proof->mutex);
        proof->wake.wait(guard, [&] { return proof->allow_initialize; }); configured = true;
    }
    void pump() override { ++proof->pumped; }
    void stop() override {
        stop_requested = true; proof->stop_entered = true; proof->wake.notify_all();
        std::unique_lock<std::mutex> guard(proof->mutex);
        proof->wake.wait(guard, [&] { return proof->allow_stop; });
    }
    void close(MdDeckLinkShutdown& receipt) noexcept override {
        if (refuse_close) { quarantine(receipt); return; }
        stop(); receipt.requested = 1; receipt.stopped = 1; finish_common(receipt, true);
    }
};
void release_gates(const std::shared_ptr<ProbeState>& proof) {
    { std::lock_guard<std::mutex> guard(proof->mutex); proof->allow_initialize = true; proof->allow_stop = true; }
    proof->wake.notify_all();
}
}
int main() {
    try {
        // Allocation exists but thread creation did not occur: same ownership
        // state as std::thread throwing before initialization acquires COM.
        auto proof = std::make_shared<ProbeState>();
        auto* unstarted = new ProbeOwner(proof); MdDeckLinkShutdown receipt{};
        check(consume(unstarted, &receipt) == 0 && proof->destroyed == 1, "unstarted owner not consumed");

        proof = std::make_shared<ProbeState>(); proof->allow_initialize = false;
        auto* late = new ProbeOwner(proof); bool timed_out = false;
        try { late->launch(std::chrono::milliseconds(10)); } catch (const Failure&) { timed_out = true; }
        check(timed_out && proof->destroyed == 0, "initialization timeout destroyed live owner");
        late->signal_stop(); release_gates(proof);
        check(consume(late, &receipt) == 0 && proof->destroyed == 1, "late initialization could not be safely retired");

        proof = std::make_shared<ProbeState>(); proof->allow_stop = false;
        auto* blocked = new ProbeOwner(proof); blocked->launch(); blocked->signal_stop();
        {
            std::unique_lock<std::mutex> guard(proof->mutex);
            check(proof->wake.wait_for(guard, std::chrono::seconds(1), [&] { return proof->stop_entered.load(); }), "stop fault was not entered");
        }
        const auto before = std::chrono::steady_clock::now(); blocked->signal_stop();
        check(std::chrono::steady_clock::now() - before < std::chrono::milliseconds(50), "stop signal waited for native stop");
        auto completion = std::async(std::launch::async, [&] { return consume(blocked, &receipt); });
        check(completion.wait_for(std::chrono::milliseconds(15)) == std::future_status::timeout
            && proof->destroyed == 0, "bounded caller timeout released blocked owner");
        release_gates(proof); check(completion.get() == 0 && proof->destroyed == 1, "blocked stop retirement failed");

        proof = std::make_shared<ProbeState>(); auto* failed = new ProbeOwner(proof);
        failed->refuse_close = true; failed->launch();
        check(consume(failed, &receipt) != 0 && !receipt.worker_joined && !receipt.released
            && receipt.outstanding_resources && proof->destroyed == 0, "failed close did not quarantine complete owner");
        const auto pumped = proof->pumped.load(); failed->signal_stop();
        std::this_thread::sleep_for(std::chrono::milliseconds(15));
        check(proof->pumped == pumped, "quarantined owner kept pumping");
        // Only this deterministic test owner is re-enabled for probe teardown.
        failed->invoke([&] { failed->refuse_close = false; failed->quarantined = false; });
        check(consume(failed, &receipt) == 0 && proof->destroyed == 1, "test quarantine cleanup failed");
        std::cout << "{\"qualifying\":false,\"native_owner_fault_tests\":\"passed\",\"cases\":[\"unstarted-owner\",\"initialization-timeout\",\"nonblocking-stop\",\"blocked-stop-owner-retention\",\"quarantine-retains-and-parks\"]}\n";
        return 0;
    } catch (const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
