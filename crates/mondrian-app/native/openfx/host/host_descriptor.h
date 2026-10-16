// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause
#ifndef HOST_DEMO_HOST_DESCRIPTOR_H
#define HOST_DEMO_HOST_DESCRIPTOR_H

#include "thread_suite.h"
#include "aux_suite.h"

namespace MondrianOpenFx {

  /// a host combines several things...
  ///    - a factory to create a new instance of your plugin
  ///      - it also gets to filter some calls during in the
  ///        API to check for validity and perform custom
  ///        operations (eg: add extra properties).
  ///    - it provides a description of the host application
  ///      which is passed back to the plugin.
  class Host : public OFX::Host::ImageEffect::Host
  {
    // Reverse destruction joins callback computation before auxiliary facts die.
    AuxSuite _aux;
    ThreadSuite _threads;
  public:
    // Per descriptor (and its instance), including Group, Page and PushButton.
    static constexpr int kMaxParameters = 4096;
    Host();
    AuxSuite& auxiliary() noexcept { return _aux; }
    const void* fetchSuite(const char*, int) override;
    OfxStatus multiThread(OfxThreadFunctionV1 function, unsigned count, void* argument) override { return _threads.run(function, count, argument); }
    OfxStatus multiThreadNumCPUS(unsigned* output) const override { return _threads.numCPUs(output); }
    OfxStatus multiThreadIndex(unsigned* output) const override { return _threads.index(output); }
    int multiThreadIsSpawnedThread() const override { return _threads.isSpawned(); }
    OfxStatus mutexCreate(OfxMutexHandle* output, int count) override { return _threads.createMutex(output, count); }
    OfxStatus mutexDestroy(OfxMutexHandle handle) override { return _threads.destroyMutex(handle); }
    OfxStatus mutexLock(OfxMutexHandle handle) override { return _threads.lockMutex(handle); }
    OfxStatus mutexUnLock(OfxMutexHandle handle) override { return _threads.unlockMutex(handle); }
    OfxStatus mutexTryLock(OfxMutexHandle handle) override { return _threads.tryMutex(handle); }

    /// Create a new instance of an image effect plug-in.
    ///
    /// It is called by ImageEffectPlugin::createInstance which the
    /// client code calls when it wants to make a new instance.
    ///
    ///   \arg clientData - the clientData passed into the ImageEffectPlugin::createInstance
    ///   \arg plugin - the plugin being created
    ///   \arg desc - the descriptor for that plugin
    ///   \arg context - the context to be created in
    virtual OFX::Host::ImageEffect::Instance* newInstance(void* clientData,
                                                          OFX::Host::ImageEffect::ImageEffectPlugin* plugin,
                                                          OFX::Host::ImageEffect::Descriptor& desc,
                                                          const std::string& context);

    /// Override this to create a descriptor, this makes the 'root' descriptor
    virtual OFX::Host::ImageEffect::Descriptor *makeDescriptor(OFX::Host::ImageEffect::ImageEffectPlugin* plugin);

    /// used to construct a context description, rootContext is the main context
    virtual OFX::Host::ImageEffect::Descriptor *makeDescriptor(const OFX::Host::ImageEffect::Descriptor &rootContext,
                                                               OFX::Host::ImageEffect::ImageEffectPlugin *plug);

    /// used to construct populate the cache
    virtual OFX::Host::ImageEffect::Descriptor *makeDescriptor(const std::string &bundlePath,
                                                               OFX::Host::ImageEffect::ImageEffectPlugin *plug);

    /// vmessage
    virtual OfxStatus vmessage(const char* type,
                               const char* id,
                               const char* format,
                               va_list args);

    /// vmessage
    virtual OfxStatus setPersistentMessage(const char* type,
                                           const char* id,
                                           const char* format,
                                           va_list args);
    /// vmessage
    virtual OfxStatus clearPersistentMessage();

#ifdef OFX_SUPPORTS_OPENGLRENDER
    /// @see OfxImageEffectOpenGLRenderSuiteV1.flushResources()
    virtual OfxStatus flushOpenGLResources() const { return kOfxStatFailed; };
#endif
  };

  // my ofx host object
  extern Host gOfxHost;

}

#endif // HOST_DEMO_HOST_DESCRIPTOR_H
