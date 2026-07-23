defineHandler({
  onEnter(log, args, state) {
    state.a = args[0];
    state.b = args[1];
    state.options = args[2];
  },

  onLeave(log, retval, state) {
    log(
      `IOReportCreateSamplesDelta(a=${state.a}, b=${state.b}, ` +
        `options=${state.options}) -> delta=${retval}`,
    );
  },
});
