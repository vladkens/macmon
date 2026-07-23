defineHandler({
  onEnter(log, args, state) {
    state.subscription = args[0];
    state.channels = args[1];
    state.options = args[2];
  },

  onLeave(log, retval, state) {
    log(
      `IOReportCreateSamples(subscription=${state.subscription}, ` +
        `channels=${state.channels}, options=${state.options}) -> sample=${retval}`,
    );
  },
});
