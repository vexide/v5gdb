// This file is built by CI and uses every part of the public C++ API to type-check v5gdb.h
// (including its templates) with `-fsyntax-only`. This file is never linked or run.

#include <v5gdb/v5gdb.h>

namespace {

class LoopbackTransport: public v5gdb::Transport {
  public:
    void session_start() noexcept override {}

    v5gdb::Result<void>
    write(std::span<std::uint8_t const> buffer) noexcept override {
        if (buffer.empty()) {
            return std::unexpected("empty write");
        }
        last_ = buffer.back();
        return {};
    }

    v5gdb::Result<void> flush() noexcept override {
        return {};
    }

    v5gdb::Result<std::optional<std::uint8_t>> peek() noexcept override {
        return last_;
    }

    v5gdb::Result<std::uint8_t> read() noexcept override {
        if (!last_) {
            return std::unexpected("no data");
        }
        return *std::exchange(last_, std::nullopt);
    }

  private:
    std::optional<std::uint8_t> last_;
};

} // namespace

void use_stdio() {
    v5gdb::set_max_log_level(v5gdb::LogFilter::Debug);
    v5gdb::install();
    v5gdb::install(
        {.stop_motors_on_break = true, .log_level = v5gdb::LogFilter::Info}
    );
    v5gdb::install({}, v5gdb::StdioTransport {});
}

void use_custom() {
    v5gdb::install({}, LoopbackTransport {});

    LoopbackTransport transport;
    v5gdb::install({.stop_motors_on_break = true}, std::move(transport));
}

void use_breakpoints() {
    if (v5gdb::is_installed()) {
        v5gdb::breakpoint();
    }
    v5gdb::breakpoint_unchecked();
}
